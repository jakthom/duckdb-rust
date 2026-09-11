//! Capture closed SQL syntax as an owned catalog expression without evaluation.
use super::*;
use crate::catalog::expression::{
    StoredArgument, StoredArgumentStyle, StoredExpression, StoredExpressionKind,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SqlBinder {
    /// Capture the representable closed-scalar SQL subset. This preserves syntax
    /// and declared literal metadata but does not resolve or execute functions,
    /// casts or operators. Callers bind the returned tree separately when their
    /// DDL contract requires dependency/type validation.
    pub fn capture_stored_expression(
        &self,
        expression: &ast::Expr,
        context: &BindContext<'_>,
    ) -> Result<StoredExpression> {
        let state = State {
            context,
            parameters_allowed: false,
            ctes: BTreeMap::new(),
            outer: Vec::new(),
        };
        let expression = state.capture_stored_expression(expression)?;
        expression.validate(context.query)?;
        Ok(expression)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn capture_stored_expression(
        &self,
        expression: &ast::Expr,
    ) -> Result<StoredExpression> {
        self.context.query.check()?;
        if let ast::Expr::Nested(expression) = expression {
            return self.capture_stored_expression(expression);
        }
        if let Some(expression) = super::nested::capture::capture(expression, &mut |child| {
            self.capture_stored_expression(child)
        })? {
            return Ok(expression);
        }
        match expression {
            ast::Expr::Value(value) if !matches!(&value.value, ast::Value::Placeholder(_)) => {
                self.capture_literal(expression)
            }
            ast::Expr::UnaryOp {
                op: ast::UnaryOperator::Minus,
                expr,
            } if matches!(expr.as_ref(), ast::Expr::Value(value) if matches!(&value.value, ast::Value::Number(_, _))) => {
                self.capture_literal(expression)
            }
            ast::Expr::TypedString(typed) => Ok(StoredExpression {
                alias: None,
                kind: StoredExpressionKind::Cast {
                    expression: Box::new(
                        self.capture_literal(&ast::Expr::Value(typed.value.clone()))?,
                    ),
                    target: self.data_type(&typed.data_type)?,
                    try_cast: false,
                },
            }),
            ast::Expr::Cast {
                kind,
                expr,
                data_type,
                array: false,
                format: None,
            } => Ok(StoredExpression {
                alias: None,
                kind: StoredExpressionKind::Cast {
                    expression: Box::new(self.capture_stored_expression(expr)?),
                    target: self.data_type(data_type)?,
                    try_cast: matches!(kind, ast::CastKind::TryCast | ast::CastKind::SafeCast),
                },
            }),
            ast::Expr::Function(function) => self.capture_stored_function(function),
            ast::Expr::BinaryOp { left, op, right } if retained_binary_operator(op).is_some() => {
                self.capture_stored_operator(
                    retained_binary_operator(op).expect("guarded operator"),
                    [left.as_ref(), right.as_ref()],
                )
            }
            ast::Expr::UnaryOp { op, expr } if retained_unary_operator(op).is_some() => self
                .capture_stored_operator(
                    retained_unary_operator(op).expect("guarded operator"),
                    [expr.as_ref()],
                ),
            ast::Expr::Value(_) => Err(unsupported("parameters in stored expressions")),
            ast::Expr::Identifier(_)
            | ast::Expr::CompoundIdentifier(_)
            | ast::Expr::Subquery(_)
            | ast::Expr::Exists { .. }
            | ast::Expr::InSubquery { .. } => {
                Err(unsupported("row or subquery dependent stored expression"))
            }
            _ => Err(unsupported(format!(
                "stored expression syntax {expression}"
            ))),
        }
    }

    fn capture_literal(&self, expression: &ast::Expr) -> Result<StoredExpression> {
        let bound = self.sql_literal(expression)?;
        let ExprKind::Literal(value) = bound.kind else {
            return Err(Error::Internal(
                "SQL literal capture produced a non-literal expression".into(),
            ));
        };
        Ok(StoredExpression::literal(bound.data_type, value))
    }

    fn capture_stored_function(&self, function: &ast::Function) -> Result<StoredExpression> {
        if function.uses_odbc_syntax
            || function.over.is_some()
            || function.filter.is_some()
            || function.null_treatment.is_some()
            || !function.within_group.is_empty()
        {
            return Err(unsupported("aggregate or window stored expression"));
        }
        let name = function
            .name
            .0
            .iter()
            .map(|part| {
                part.as_ident()
                    .map(|identifier| identifier.value.clone())
                    .ok_or_else(|| unsupported("stored function name expression"))
            })
            .collect::<Result<Vec<_>>>()?;
        let leaf = name
            .last()
            .ok_or_else(|| unsupported("empty stored function name"))?;
        if self.context.functions.aggregate(leaf).is_some() {
            return Err(unsupported("aggregate stored expression"));
        }
        let parsed = super::nested::scalar_arguments(function)?;
        let arguments = parsed
            .expressions
            .iter()
            .enumerate()
            .map(|(index, child)| {
                let mut expression = self.capture_stored_expression(child)?;
                expression.alias = parsed.aliases[index].clone();
                Ok(StoredArgument {
                    name: parsed.names[index].clone(),
                    expression,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(StoredExpression {
            alias: None,
            kind: StoredExpressionKind::Function {
                name,
                arguments,
                is_operator: false,
                argument_style: StoredArgumentStyle::Named,
            },
        })
    }

    fn capture_stored_operator<'a>(
        &self,
        name: &'static str,
        children: impl IntoIterator<Item = &'a ast::Expr>,
    ) -> Result<StoredExpression> {
        Ok(StoredExpression {
            alias: None,
            kind: StoredExpressionKind::Function {
                name: vec![name.into()],
                arguments: children
                    .into_iter()
                    .map(|child| {
                        Ok(StoredArgument {
                            name: None,
                            expression: self.capture_stored_expression(child)?,
                        })
                    })
                    .collect::<Result<_>>()?,
                is_operator: true,
                argument_style: StoredArgumentStyle::Named,
            },
        })
    }
}

fn retained_binary_operator(operator: &ast::BinaryOperator) -> Option<&'static str> {
    use ast::BinaryOperator as O;
    Some(match operator {
        O::Plus => "+",
        O::Minus => "-",
        O::Multiply => "*",
        O::Divide => "/",
        O::DuckIntegerDivide => "//",
        O::Modulo => "%",
        O::StringConcat => "||",
        O::BitwiseAnd => "&",
        O::BitwiseOr => "|",
        O::PGBitwiseShiftLeft => "<<",
        O::PGBitwiseShiftRight => ">>",
        O::Eq => "=",
        O::NotEq => "!=",
        O::Lt => "<",
        O::LtEq => "<=",
        O::Gt => ">",
        O::GtEq => ">=",
        O::And => "and",
        O::Or => "or",
        _ => return None,
    })
}

fn retained_unary_operator(operator: &ast::UnaryOperator) -> Option<&'static str> {
    use ast::UnaryOperator as O;
    Some(match operator {
        O::Plus => "+",
        O::Minus => "-",
        O::Not => "not",
        O::BitwiseNot => "~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::expression_executor::ScalarEvaluator,
        function::{FunctionRegistry, operator::OperatorRegistry},
        parallel::QueryContext,
        parser::{DuckDbParser, Parser, Statement},
        storage::table::Snapshot,
    };

    fn parsed(sql: &str) -> Result<ast::Expr> {
        let Statement::Sql(statement) = DuckDbParser.parse(&format!("SELECT {sql}"))?.remove(0)
        else {
            unreachable!()
        };
        let ast::Statement::Query(query) = *statement else {
            unreachable!()
        };
        let ast::SetExpr::Select(select) = *query.body else {
            unreachable!()
        };
        let ast::SelectItem::UnnamedExpr(expression) = select.projection[0].clone() else {
            unreachable!()
        };
        Ok(expression)
    }

    fn capture(sql: &str) -> Result<StoredExpression> {
        let query = QueryContext::background();
        let catalog = Snapshot::new(query.type_registry());
        let casts = crate::common::cast::CastRegistry::builtins();
        let operators = OperatorRegistry::builtins();
        let functions = FunctionRegistry::builtins();
        let expressions = ScalarEvaluator;
        SqlBinder.capture_stored_expression(
            &parsed(sql)?,
            &BindContext {
                catalog: &catalog,
                casts: &casts,
                operators: &operators,
                query: &query,
                functions: &functions,
                expressions: &expressions,
                parameters: &[],
            },
        )
    }

    #[test]
    fn capture_preserves_literal_cast_function_and_operator_syntax_without_evaluation() -> Result<()>
    {
        assert!(matches!(
            capture("1")?.kind,
            StoredExpressionKind::Literal {
                data_type: DataType::Integer,
                value: Value::Integer(1)
            }
        ));
        assert!(matches!(
            capture("TRY_CAST('bad' AS SMALLINT)")?.kind,
            StoredExpressionKind::Cast {
                target: DataType::SmallInt,
                try_cast: true,
                ..
            }
        ));
        let function = capture("catalog.schema.not_executed(value := 'LOUD')")?;
        let StoredExpressionKind::Function {
            name,
            arguments,
            is_operator,
            ..
        } = function.kind
        else {
            unreachable!()
        };
        assert_eq!(name, ["catalog", "schema", "not_executed"]);
        assert!(!is_operator);
        assert_eq!(arguments[0].name.as_deref(), Some("value"));
        assert_eq!(arguments[0].expression.alias.as_deref(), Some("value"));
        let operator = capture("1 + 2")?;
        assert!(matches!(
            operator.kind,
            StoredExpressionKind::Function {
                ref name,
                is_operator: true,
                ..
            } if name == &["+"]
        ));
        Ok(())
    }

    #[test]
    fn capture_rejects_dependencies_parameters_aggregates_and_windows() -> Result<()> {
        for sql in [
            "column_name",
            "(SELECT 1)",
            "$1",
            "sum(1)",
            "row_number() OVER ()",
        ] {
            assert!(matches!(capture(sql), Err(Error::Unsupported(_))), "{sql}");
        }
        Ok(())
    }
}
