use super::*;

const MAX_GROUPING_SETS: usize = 65535;

pub(super) struct BoundGroups {
    pub groups: Vec<(ast::Expr, BoundExpr)>,
    pub sets: Vec<GroupingSet>,
    pub explicit: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn group_by(
        &self,
        clause: &ast::GroupByExpr,
        fields: &Scope,
        items: &[SelectItem],
    ) -> Result<BoundGroups> {
        let ast::GroupByExpr::Expressions(expressions, modifiers) = clause else {
            return Err(unsupported("GROUP BY ALL"));
        };
        if !modifiers.is_empty() {
            return Err(unsupported("GROUP BY modifiers"));
        }
        let mut result = BoundGroups {
            groups: Vec::new(),
            sets: Vec::new(),
            explicit: !expressions.is_empty(),
        };
        result.sets = self.group_product(expressions, fields, items, &mut result.groups, 0)?;
        Ok(result)
    }

    fn group_product(
        &self,
        expressions: &[ast::Expr],
        fields: &Scope,
        items: &[SelectItem],
        groups: &mut Vec<(ast::Expr, BoundExpr)>,
        depth: usize,
    ) -> Result<Vec<GroupingSet>> {
        let mut sets = vec![GroupingSet::new([])];
        for expression in expressions {
            let other = self.group_item(expression, fields, items, groups, depth + 1)?;
            check_count(sets.len().saturating_mul(other.len()))?;
            let mut product = Vec::new();
            for a in &sets {
                for b in &other {
                    self.context.query.check()?;
                    product.push(merge(a, b));
                }
            }
            sets = product;
        }
        Ok(sets)
    }

    fn group_item(
        &self,
        expression: &ast::Expr,
        fields: &Scope,
        items: &[SelectItem],
        groups: &mut Vec<(ast::Expr, BoundExpr)>,
        depth: usize,
    ) -> Result<Vec<GroupingSet>> {
        self.context.query.check()?;
        if depth > 128 {
            return Err(Error::Resource("grouping nesting exceeds 128".into()));
        }
        match expression {
            ast::Expr::GroupingSets(alternatives) => {
                if alternatives.is_empty() {
                    return Err(Error::Parse(
                        "GROUPING SETS requires at least one set".into(),
                    ));
                }
                let mut result = Vec::new();
                for alternative in alternatives {
                    let alternative = alternative
                        .iter()
                        .map(grouping_construct)
                        .collect::<Result<Vec<_>>>()?;
                    let sets =
                        self.group_product(&alternative, fields, items, groups, depth + 1)?;
                    check_count(result.len().saturating_add(sets.len()))?;
                    result.extend(sets);
                }
                Ok(result)
            }
            ast::Expr::Rollup(units) | ast::Expr::Cube(units) => {
                let mut result = vec![GroupingSet::new([])];
                let mut prefix = GroupingSet::new([]);
                for unit in units {
                    let item = self.group_product(unit, fields, items, groups, depth + 1)?;
                    let [item] = item.as_slice() else {
                        return Err(Error::Parse(
                            "grouping alternatives inside ROLLUP or CUBE".into(),
                        ));
                    };
                    if matches!(expression, ast::Expr::Rollup(_)) {
                        check_count(result.len() + 1)?;
                        prefix = merge(&prefix, item);
                        result.push(prefix.clone());
                    } else {
                        check_count(result.len().saturating_mul(2))?;
                        let count = result.len();
                        for index in 0..count {
                            self.context.query.check()?;
                            result.push(merge(&result[index], item));
                        }
                    }
                }
                Ok(result)
            }
            ast::Expr::Tuple(expressions) => {
                self.group_product(expressions, fields, items, groups, depth + 1)
            }
            ast::Expr::Nested(expression) => {
                self.group_item(expression, fields, items, groups, depth + 1)
            }
            _ => {
                let selected = if let Some(index) = ordinal(expression, items.len())? {
                    Some(&items[index])
                } else if let ast::Expr::Identifier(name) = expression {
                    match fields.resolve_optional(std::slice::from_ref(&name.value))? {
                        Some(_) => None,
                        None => items
                            .iter()
                            .find(|item| item.name.eq_ignore_ascii_case(&name.value)),
                    }
                } else {
                    None
                };
                let expression = selected
                    .map(|item| &item.expression)
                    .unwrap_or(expression)
                    .clone();
                if let Some(item) = selected.filter(|item| item.column.is_some()) {
                    let index = if let Some(index) = item.group_index(fields, groups) {
                        index
                    } else {
                        let index = groups.len();
                        groups.push((expression, item.bind(self, fields, None)?));
                        index
                    };
                    return Ok(vec![GroupingSet::new([index])]);
                }
                let index = if let Some(index) = group_index(&expression, fields, groups) {
                    index
                } else {
                    let bound = self.expr(&expression, fields, None)?;
                    let index = groups.len();
                    groups.push((expression, bound));
                    index
                };
                Ok(vec![GroupingSet::new([index])])
            }
        }
    }

    pub(super) fn grouping_function(
        &self,
        expression: &ast::Expr,
        function: &ast::Function,
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<BoundExpr> {
        let grouping = grouping
            .ok_or_else(|| Error::Bind("GROUPING function is not supported here".into()))?;
        if grouping.groups.is_empty() {
            return Err(Error::Bind(
                "GROUPING statement cannot be used without groups".into(),
            ));
        }
        if function.filter.is_some()
            || matches!(&function.args, ast::FunctionArguments::List(args) if args.duplicate_treatment.is_some())
        {
            return Err(Error::Bind(
                "GROUPING does not support FILTER or DISTINCT".into(),
            ));
        }
        let arguments = function_arguments(function)?;
        let indices = if arguments.is_empty() {
            (0..grouping.groups.len()).collect::<Vec<_>>()
        } else {
            arguments
                .iter()
                .map(|argument| {
                    grouping.index(argument, fields).ok_or_else(|| {
                        Error::Bind(format!(
                            "GROUPING child \"{argument}\" must be a grouping column"
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };
        if indices.len() > 63 {
            return Err(Error::Bind(
                "GROUPING statement cannot have more than 64 groups".into(),
            ));
        }
        let mut outputs = grouping.outputs.borrow_mut();
        let index = if let Some(index) = outputs.iter().position(|(e, _)| e == expression) {
            index
        } else {
            let index = outputs.len();
            outputs.push((expression.clone(), AggregateOutput::Grouping(indices)));
            index
        };
        Ok(BoundExpr::column(
            grouping.groups.len() + index,
            DataType::BigInt,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupScope {
    pub(super) fn index(&self, expression: &ast::Expr, fields: &Scope) -> Option<usize> {
        group_index(expression, fields, &self.groups).or_else(|| match expression {
            ast::Expr::Identifier(name)
                if matches!(
                    fields.resolve_optional(std::slice::from_ref(&name.value)),
                    Ok(None)
                ) =>
            {
                self.aliases.get(&name.value.to_ascii_lowercase()).copied()
            }
            _ => None,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn group_index(
    expression: &ast::Expr,
    fields: &Scope,
    groups: &[(ast::Expr, BoundExpr)],
) -> Option<usize> {
    if let ast::Expr::Nested(expression) = expression {
        return group_index(expression, fields, groups);
    }
    let parts = match expression {
        ast::Expr::Identifier(name) => Some(vec![name.value.clone()]),
        ast::Expr::CompoundIdentifier(names) => {
            Some(names.iter().map(|name| name.value.clone()).collect())
        }
        _ => None,
    };
    let column = match parts {
        Some(parts) => fields.resolve_optional(&parts).ok()?,
        None => None,
    };
    groups.iter().position(|(ast, bound)| {
        ast == expression
            || column.is_some_and(
                |column| matches!(bound.kind, ExprKind::Column(index) if index == column),
            )
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn merge(a: &GroupingSet, b: &GroupingSet) -> GroupingSet {
    GroupingSet::new(a.indices().iter().chain(b.indices()).copied())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// The upstream grammar represents ROLLUP/CUBE within GROUPING SETS as
/// function calls. Interpret them only at that grammar boundary: inside a
/// ROLLUP/CUBE unit they are ordinary scalar expressions, not more sets.
fn grouping_construct(expression: &ast::Expr) -> Result<ast::Expr> {
    let ast::Expr::Function(function) = expression else {
        return Ok(expression.clone());
    };
    let name = function.name.to_string();
    if function.name.0.len() != 1
        || function.name.0[0]
            .as_ident()
            .is_none_or(|name| name.quote_style.is_some())
        || !(name.eq_ignore_ascii_case("rollup") || name.eq_ignore_ascii_case("cube"))
    {
        return Ok(expression.clone());
    }
    if function.filter.is_some()
        || function.over.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || matches!(&function.args, ast::FunctionArguments::List(args) if args.duplicate_treatment.is_some())
    {
        return Err(Error::Parse("invalid grouping construct".into()));
    }
    let units = function_arguments(function)?
        .into_iter()
        .map(|argument| vec![argument])
        .collect::<Vec<_>>();
    if units.is_empty() {
        return Err(Error::Parse("ROLLUP and CUBE require an argument".into()));
    }
    Ok(if name.eq_ignore_ascii_case("rollup") {
        ast::Expr::Rollup(units)
    } else {
        ast::Expr::Cube(units)
    })
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_count(count: usize) -> Result<()> {
    if count > MAX_GROUPING_SETS {
        Err(Error::Parse(format!(
            "Maximum grouping set count of {MAX_GROUPING_SETS} exceeded"
        )))
    } else {
        Ok(())
    }
}
