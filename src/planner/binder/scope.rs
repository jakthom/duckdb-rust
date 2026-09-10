use super::*;

/// SQL name visibility is separate from the operator's physical row layout.
/// USING keeps qualified source keys while exposing one unqualified key.
#[derive(Clone, Default)]
pub(super) struct Scope {
    fields: Schema,
    pub visible: Vec<usize>,
    using: Vec<usize>,
    pub windows: Option<std::rc::Rc<window::WindowScope>>,
    pub aliases: BTreeMap<String, Vec<BoundExpr>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl From<Schema> for Scope {
    fn from(fields: Schema) -> Self {
        Self {
            visible: (0..fields.len()).collect(),
            fields,
            using: Vec::new(),
            windows: None,
            aliases: BTreeMap::new(),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::ops::Deref for Scope {
    type Target = [Field];
    fn deref(&self) -> &Self::Target {
        &self.fields
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Scope {
    pub fn resolve_optional(&self, parts: &[String]) -> Result<Option<usize>> {
        if parts.len() != 1 {
            return resolve_optional(&self.fields, parts);
        }
        let using = self
            .using
            .iter()
            .any(|&index| self.fields[index].name.eq_ignore_ascii_case(&parts[0]));
        let candidates = if using { &self.using } else { &self.visible };
        let mut matches = candidates
            .iter()
            .copied()
            .filter(|&index| self.fields[index].name.eq_ignore_ascii_case(&parts[0]));
        let first = matches.next();
        if matches.next().is_some() {
            return Err(Error::Bind(format!("ambiguous column {}", parts[0])));
        }
        Ok(first)
    }

    pub fn resolve(&self, parts: &[String]) -> Result<usize> {
        self.resolve_optional(parts)?
            .ok_or_else(|| Error::Bind(format!("column {} not found", parts.join("."))))
    }

    pub fn combine(&self, right: &Self) -> Result<Self> {
        for qualifier in self
            .fields
            .iter()
            .filter_map(|field| field.qualifier.as_ref())
        {
            if right
                .fields
                .iter()
                .filter_map(|field| field.qualifier.as_ref())
                .any(|other| other.eq_ignore_ascii_case(qualifier))
            {
                return Err(Error::Bind(format!(
                    "ambiguous reference to table {qualifier}"
                )));
            }
        }
        let mut result = self.clone();
        result
            .visible
            .extend(right.visible.iter().map(|index| index + self.len()));
        result
            .using
            .extend(right.using.iter().map(|index| index + self.len()));
        result.fields.extend(right.fields.clone());
        Ok(result)
    }

    pub fn merge_key(&mut self, left: usize, right: usize, key: usize) {
        self.visible.retain(|&index| index != right);
        for index in &mut self.visible {
            if *index == left {
                *index = key;
            }
        }
        self.using.retain(|&index| index != left && index != right);
        self.using.push(key);
    }

    pub fn truncate(&mut self, width: usize) {
        self.fields.truncate(width);
        self.visible.retain(|&index| index < width);
        self.using.retain(|&index| index < width);
    }

    pub fn append(&mut self, field: Field) -> usize {
        let index = self.fields.len();
        self.fields.push(field);
        index
    }

    pub fn star(&self, qualifier: Option<&str>) -> Result<Vec<SelectItem>> {
        let columns: Vec<_> = match qualifier {
            None => self.visible.clone(),
            Some(name) => self
                .fields
                .iter()
                .enumerate()
                .filter_map(|(index, field)| {
                    field
                        .qualifier
                        .as_ref()
                        .is_some_and(|q| q.eq_ignore_ascii_case(name))
                        .then_some(index)
                })
                .collect(),
        };
        if let Some(qualifier) = qualifier
            && columns.is_empty()
        {
            return Err(Error::Bind(format!("table {} not found", qualifier)));
        }
        Ok(columns
            .into_iter()
            .map(|index| {
                let field = &self.fields[index];
                let expression = if let Some(qualifier) = &field.qualifier {
                    ast::Expr::CompoundIdentifier(vec![
                        ast::Ident::new(qualifier),
                        ast::Ident::new(&field.name),
                    ])
                } else {
                    ast::Expr::Identifier(ast::Ident::new(&field.name))
                };
                SelectItem {
                    expression,
                    name: field.name.clone(),
                    column: Some(index),
                }
            })
            .collect())
    }
}

pub(super) struct Relation {
    pub plan: LogicalPlan,
    pub scope: Scope,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl From<LogicalPlan> for Relation {
    fn from(plan: LogicalPlan) -> Self {
        Self {
            scope: Scope::from(plan.schema.clone()),
            plan,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Relation {
    /// A derived relation exposes its visible result; source namespaces end here.
    pub fn project_visible(self) -> LogicalPlan {
        if self
            .scope
            .visible
            .iter()
            .copied()
            .eq(0..self.plan.schema.len())
        {
            return self.plan;
        }
        let schema: Schema = self
            .scope
            .visible
            .iter()
            .map(|&i| self.plan.schema[i].clone())
            .collect();
        let expressions = self
            .scope
            .visible
            .iter()
            .map(|&i| BoundExpr::column(i, self.plan.schema[i].data_type.clone()))
            .collect();
        LogicalPlan {
            schema,
            node: PlanNode::Projection {
                input: Box::new(self.plan),
                expressions,
            },
        }
    }
}

/// Wildcards have already resolved their column identities. Keeping that
/// identity avoids rebinding duplicate or merged names as ambiguous SQL text.
pub(super) struct SelectItem {
    pub expression: ast::Expr,
    pub name: String,
    pub column: Option<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SelectItem {
    pub fn expression(expression: ast::Expr, name: String) -> Self {
        Self {
            expression,
            name,
            column: None,
        }
    }

    pub fn group_index(&self, fields: &Scope, groups: &[(ast::Expr, BoundExpr)]) -> Option<usize> {
        match self.column {
            Some(column) => groups.iter().position(|(_, expression)| matches!(expression.kind, ExprKind::Column(index) if index == column)),
            None => grouping::group_index(&self.expression, fields, groups),
        }
    }

    pub fn bind(
        &self,
        state: &State<'_, '_>,
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<BoundExpr> {
        let Some(column) = self.column else {
            return state.expr(&self.expression, fields, grouping);
        };
        let index = match grouping {
            None => column,
            Some(grouping) => self.group_index(fields, &grouping.groups).ok_or_else(|| {
                Error::Bind(format!("column {} must appear in GROUP BY", self.name))
            })?,
        };
        Ok(BoundExpr::column(index, fields[column].data_type.clone()))
    }
}
