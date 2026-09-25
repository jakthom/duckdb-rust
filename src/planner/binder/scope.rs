use super::*;

/// SQL name visibility is separate from the operator's physical row layout.
/// USING keeps qualified source keys while exposing one unqualified key.
#[derive(Clone, Default)]
pub(super) struct Scope {
    fields: Schema,
    bindings: Vec<Binding>,
    pub visible: Vec<usize>,
    using: Vec<usize>,
    merged_keys: BTreeMap<usize, (usize, usize)>,
    pub windows: Option<std::rc::Rc<window::WindowScope>>,
    pub aliases: BTreeMap<String, Vec<BoundExpr>>,
}

/// A SQL relation namespace, independent of the physical plan's field labels.
/// Identifier components stay separate: a quoted dot is not a schema separator.
#[derive(Clone)]
struct Binding {
    qualifier: Vec<String>,
    columns: std::ops::Range<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Binding {
    fn matches(&self, qualifier: &[String]) -> bool {
        qualifier.len() <= self.qualifier.len()
            && self.qualifier[self.qualifier.len() - qualifier.len()..]
                .iter()
                .zip(qualifier)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl From<Schema> for Scope {
    fn from(fields: Schema) -> Self {
        let mut bindings: Vec<Binding> = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            let Some(qualifier) = &field.qualifier else {
                continue;
            };
            if let Some(last) = bindings.last_mut()
                && last.columns.end == index
                && last.matches(std::slice::from_ref(qualifier))
            {
                last.columns.end += 1;
            } else {
                bindings.push(Binding {
                    qualifier: vec![qualifier.clone()],
                    columns: index..index + 1,
                });
            }
        }
        Self {
            visible: (0..fields.len()).collect(),
            fields,
            bindings,
            using: Vec::new(),
            merged_keys: BTreeMap::new(),
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
    pub fn qualify_table(&mut self, table: &TableName) {
        self.bindings = vec![Binding {
            qualifier: vec![table.schema.clone(), table.name.clone()],
            columns: 0..self.fields.len(),
        }];
    }

    pub fn resolve_optional(&self, parts: &[String]) -> Result<Option<usize>> {
        if parts.len() != 1 {
            if parts.is_empty() {
                return Err(Error::Bind("empty column name".into()));
            }
            if parts.len() > 3 {
                return Err(unsupported("cross-database column names"));
            }
            let mut matches = self
                .bindings
                .iter()
                .filter(|binding| binding.matches(&parts[..parts.len() - 1]))
                .flat_map(|binding| binding.columns.clone())
                .filter(|&index| {
                    self.fields[index]
                        .name
                        .eq_ignore_ascii_case(&parts[parts.len() - 1])
                });
            let first = matches.next();
            if matches.next().is_some() {
                return Err(Error::Bind(format!("ambiguous column {}", parts.join("."))));
            }
            return Ok(first);
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

    /// Resolve a relation namespace as the contiguous columns used by
    /// table-as-STRUCT binding. A suffix qualifier is valid, but two matching
    /// namespaces remain ambiguous.
    pub fn relation_columns_optional(&self, name: &str) -> Result<Option<std::ops::Range<usize>>> {
        let qualifier = [name.to_owned()];
        let mut matches = self
            .bindings
            .iter()
            .filter(|binding| binding.matches(&qualifier));
        let first = matches.next().map(|binding| binding.columns.clone());
        if matches.next().is_some() {
            return Err(Error::Bind(format!("ambiguous reference to table {name}")));
        }
        Ok(first)
    }

    pub fn resolve(&self, parts: &[String]) -> Result<usize> {
        self.resolve_optional(parts)?
            .ok_or_else(|| Error::Bind(format!("column {} not found", parts.join("."))))
    }

    /// Prefer the most qualified column before interpreting trailing names as
    /// nested fields. Table bindings currently have at most schema + table;
    /// a longer path may still be a supported column followed by many fields.
    pub fn resolve_prefix(&self, parts: &[String]) -> Result<Option<(usize, usize)>> {
        if parts.is_empty() {
            return Err(Error::Bind("empty column name".into()));
        }
        for length in (1..=parts.len().min(3)).rev() {
            if let Some(index) = self.resolve_optional(&parts[..length])? {
                return Ok(Some((index, length)));
            }
        }
        Ok(None)
    }

    pub fn combine(&self, right: &Self) -> Self {
        let mut result = self.clone();
        result
            .bindings
            .extend(right.bindings.iter().map(|binding| Binding {
                qualifier: binding.qualifier.clone(),
                columns: binding.columns.start + self.len()..binding.columns.end + self.len(),
            }));
        result
            .visible
            .extend(right.visible.iter().map(|index| index + self.len()));
        result
            .using
            .extend(right.using.iter().map(|index| index + self.len()));
        result
            .merged_keys
            .extend(right.merged_keys.iter().map(|(&key, &(left, right))| {
                (key + self.len(), (left + self.len(), right + self.len()))
            }));
        result.fields.extend(right.fields.clone());
        result
    }

    pub fn merge_key(&mut self, left: usize, right: usize, key: usize) {
        if key != left && key != right {
            self.merged_keys.insert(key, (left, right));
        }
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
        self.bindings
            .retain(|binding| binding.columns.start < width);
        for binding in &mut self.bindings {
            binding.columns.end = binding.columns.end.min(width);
        }
        self.visible.retain(|&index| index < width);
        self.using.retain(|&index| index < width);
        self.merged_keys.retain(|key, _| *key < width);
    }

    pub fn append(&mut self, field: Field) -> usize {
        let index = self.fields.len();
        self.fields.push(field);
        index
    }

    pub fn star(&self, qualifier: Option<&str>) -> Result<Vec<SelectItem>> {
        let columns: Vec<_> = match qualifier {
            None => {
                // C++ expands an unqualified star to qualified references. Two
                // identical namespaces may coexist, but a shared column cannot
                // be resolved merely by keeping its physical row index.
                for &index in &self.visible {
                    self.validate_star_column(index)?;
                }
                self.visible.clone()
            }
            Some(name) => {
                let qualifier = [name.to_owned()];
                let mut matches = self
                    .bindings
                    .iter()
                    .filter(|binding| binding.matches(&qualifier));
                let first = matches
                    .next()
                    .ok_or_else(|| Error::Bind(format!("table {name} not found")))?;
                if matches.next().is_some() {
                    return Err(Error::Bind(format!("ambiguous reference to table {name}")));
                }
                first.columns.clone().collect()
            }
        };
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

    fn validate_star_column(&self, index: usize) -> Result<()> {
        if let Some(&(left, right)) = self.merged_keys.get(&index) {
            self.validate_star_column(left)?;
            return self.validate_star_column(right);
        }
        if let Some(binding) = self
            .bindings
            .iter()
            .find(|binding| binding.columns.contains(&index))
        {
            let count = self
                .bindings
                .iter()
                .filter(|other| {
                    other.matches(&binding.qualifier)
                        && other.columns.clone().any(|column| {
                            self.fields[column]
                                .name
                                .eq_ignore_ascii_case(&self.fields[index].name)
                        })
                })
                .count();
            if count > 1 {
                return Err(Error::Bind(format!(
                    "ambiguous reference to table {}",
                    binding.qualifier.join(".")
                )));
            }
        }
        Ok(())
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
