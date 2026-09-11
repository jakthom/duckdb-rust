//! Capture closed SQL syntax as an owned catalog expression without evaluation.
use super::*;
use crate::catalog::expression::{
    StoredArgument, StoredArgumentStyle, StoredCaseCheck, StoredComparison, StoredConjunction,
    StoredExpression, StoredExpressionKind, StoredOperator,
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
    /// Retain a column default after binding its closed dependencies and target
    /// assignment conversion. Binding must not execute the retained body.
    pub(super) fn capture_column_default(
        &self,
        expression: &ast::Expr,
        target: &DataType,
    ) -> Result<StoredExpression> {
        let stored = self.capture_stored_expression(expression)?;
        stored.validate(self.context.query)?;
        let bound = self.stored_expression(&stored)?;
        if !super::closed_expression(&bound) {
            return Err(unsupported(
                "stored expression with row or parameter dependencies",
            ));
        }
        bound.validate_closed(self.context.catalog, self.context.query)?;
        let assigned = bound.cast(
            target.clone(),
            CastMode::Assignment,
            self.context.casts,
            self.context.query.types(),
        )?;
        assigned.validate_closed(self.context.catalog, self.context.query)?;
        Ok(stored)
    }

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
                source_span: None,
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
                source_span: None,
                kind: StoredExpressionKind::Cast {
                    expression: Box::new(self.capture_stored_expression(expr)?),
                    target: self.data_type(data_type)?,
                    try_cast: matches!(kind, ast::CastKind::TryCast | ast::CastKind::SafeCast),
                },
            }),
            ast::Expr::Function(function) => self.capture_stored_function(function),
            ast::Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                let operand = operand
                    .as_ref()
                    .map(|operand| self.capture_stored_expression(operand))
                    .transpose()?;
                let mut checks = Vec::with_capacity(conditions.len());
                for condition in conditions {
                    let predicate = self.capture_stored_expression(&condition.condition)?;
                    checks.push(StoredCaseCheck {
                        when_expression: if let Some(operand) = &operand {
                            stored_comparison(StoredComparison::Equal, operand.clone(), predicate)
                        } else {
                            predicate
                        },
                        then_expression: self.capture_stored_expression(&condition.result)?,
                    });
                }
                Ok(StoredExpression {
                    alias: None,
                    source_span: None,
                    kind: StoredExpressionKind::Case {
                        checks,
                        otherwise: Box::new(
                            else_result
                                .as_ref()
                                .map(|expression| self.capture_stored_expression(expression))
                                .transpose()?
                                .unwrap_or_else(|| {
                                    StoredExpression::literal(DataType::Null, Value::Null)
                                }),
                        ),
                    },
                })
            }
            ast::Expr::IsNull(expression) => Ok(stored_native_operator(
                StoredOperator::IsNull,
                vec![self.capture_stored_expression(expression)?],
            )),
            ast::Expr::IsNotNull(expression) => Ok(stored_native_operator(
                StoredOperator::IsNotNull,
                vec![self.capture_stored_expression(expression)?],
            )),
            ast::Expr::Between {
                expr,
                negated,
                low,
                high,
            } => {
                let between = StoredExpression {
                    alias: None,
                    source_span: None,
                    kind: StoredExpressionKind::Between {
                        input: Box::new(self.capture_stored_expression(expr)?),
                        lower: Box::new(self.capture_stored_expression(low)?),
                        upper: Box::new(self.capture_stored_expression(high)?),
                    },
                };
                Ok(if *negated {
                    stored_native_operator(StoredOperator::Not, vec![between])
                } else {
                    between
                })
            }
            ast::Expr::InList {
                expr,
                list,
                negated,
            } => {
                let mut children = Vec::with_capacity(list.len() + 1);
                children.push(self.capture_stored_expression(expr)?);
                children.extend(
                    list.iter()
                        .map(|expression| self.capture_stored_expression(expression))
                        .collect::<Result<Vec<_>>>()?,
                );
                let expression = stored_native_operator(StoredOperator::In, children);
                Ok(if *negated {
                    stored_native_operator(StoredOperator::Not, vec![expression])
                } else {
                    expression
                })
            }
            ast::Expr::Like {
                negated,
                expr,
                pattern,
                escape_char: None,
                any: false,
            } => self.capture_stored_operator(
                if *negated { "!~~" } else { "~~" },
                [expr.as_ref(), pattern.as_ref()],
            ),
            ast::Expr::BinaryOp { left, op, right } if retained_comparison(op).is_some() => {
                Ok(stored_comparison(
                    retained_comparison(op).expect("guarded comparison"),
                    self.capture_stored_expression(left)?,
                    self.capture_stored_expression(right)?,
                ))
            }
            ast::Expr::BinaryOp { left, op, right } if retained_conjunction(op).is_some() => {
                Ok(StoredExpression {
                    alias: None,
                    source_span: None,
                    kind: StoredExpressionKind::Conjunction {
                        kind: retained_conjunction(op).expect("guarded conjunction"),
                        children: vec![
                            self.capture_stored_expression(left)?,
                            self.capture_stored_expression(right)?,
                        ],
                    },
                })
            }
            ast::Expr::BinaryOp { left, op, right } if retained_binary_operator(op).is_some() => {
                self.capture_stored_operator(
                    retained_binary_operator(op).expect("guarded operator"),
                    [left.as_ref(), right.as_ref()],
                )
            }
            ast::Expr::UnaryOp {
                op: ast::UnaryOperator::Not,
                expr,
            } => Ok(stored_native_operator(
                StoredOperator::Not,
                vec![self.capture_stored_expression(expr)?],
            )),
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
            source_span: None,
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
        Ok(stored_syntax_operator(
            name,
            children
                .into_iter()
                .map(|child| self.capture_stored_expression(child))
                .collect::<Result<_>>()?,
        ))
    }
}

fn stored_syntax_operator(name: &'static str, children: Vec<StoredExpression>) -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec![name.into()],
            arguments: children
                .into_iter()
                .map(|expression| StoredArgument {
                    name: None,
                    expression,
                })
                .collect(),
            is_operator: true,
            argument_style: StoredArgumentStyle::Named,
        },
    }
}

fn stored_native_operator(
    kind: StoredOperator,
    children: Vec<StoredExpression>,
) -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Operator { kind, children },
    }
}

fn stored_comparison(
    kind: StoredComparison,
    left: StoredExpression,
    right: StoredExpression,
) -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Comparison {
            kind,
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

fn retained_comparison(operator: &ast::BinaryOperator) -> Option<StoredComparison> {
    use ast::BinaryOperator as O;
    Some(match operator {
        O::Eq => StoredComparison::Equal,
        O::NotEq => StoredComparison::NotEqual,
        O::Lt => StoredComparison::LessThan,
        O::Gt => StoredComparison::GreaterThan,
        O::LtEq => StoredComparison::LessThanOrEqual,
        O::GtEq => StoredComparison::GreaterThanOrEqual,
        _ => return None,
    })
}

fn retained_conjunction(operator: &ast::BinaryOperator) -> Option<StoredConjunction> {
    use ast::BinaryOperator as O;
    Some(match operator {
        O::And => StoredConjunction::And,
        O::Or => StoredConjunction::Or,
        _ => return None,
    })
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
        _ => return None,
    })
}

fn retained_unary_operator(operator: &ast::UnaryOperator) -> Option<&'static str> {
    use ast::UnaryOperator as O;
    Some(match operator {
        O::Plus => "+",
        O::Minus => "-",
        O::BitwiseNot => "~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::expression_executor::{ExpressionEvaluator, ScalarEvaluator},
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
            "CASE WHEN true THEN column_name ELSE 0 END",
            "column_name IS NULL",
            "column_name BETWEEN 1 AND 2",
            "1 IN (2, column_name)",
            "column_name LIKE 'x%'",
        ] {
            assert!(matches!(capture(sql), Err(Error::Unsupported(_))), "{sql}");
        }
        Ok(())
    }

    #[test]
    fn captured_closed_conditional_and_predicate_syntax_binds_without_reparsing() -> Result<()> {
        let query = QueryContext::background();
        let catalog = Snapshot::new(query.type_registry());
        let casts = crate::common::cast::CastRegistry::builtins();
        let operators = OperatorRegistry::builtins();
        let functions = FunctionRegistry::builtins();
        let expressions = ScalarEvaluator;
        let context = BindContext {
            catalog: &catalog,
            casts: &casts,
            operators: &operators,
            query: &query,
            functions: &functions,
            expressions: &expressions,
            parameters: &[],
        };
        for (sql, expected) in [
            ("CASE WHEN 1=1 THEN 7 ELSE 0 END", Value::Integer(7)),
            (
                "CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
                Value::Varchar("two".into()),
            ),
            ("NULL IS NULL", Value::Boolean(true)),
            ("NULL IS NOT NULL", Value::Boolean(false)),
            ("2 BETWEEN 1 AND 3", Value::Boolean(true)),
            ("2 NOT BETWEEN 1 AND 3", Value::Boolean(false)),
            ("2 IN (1, 2, NULL)", Value::Boolean(true)),
            ("2 NOT IN (1, 3, NULL)", Value::Null),
            ("'duck' LIKE 'd%'", Value::Boolean(true)),
            ("'duck' NOT LIKE 'd%'", Value::Boolean(false)),
        ] {
            let stored = SqlBinder.capture_stored_expression(&parsed(sql)?, &context)?;
            let bound = SqlBinder.bind_stored_expression(&stored, &context)?;
            assert_eq!(
                expressions.evaluate(&bound, &Vec::new(), &query)?,
                expected,
                "{sql}"
            );
        }
        Ok(())
    }

    #[test]
    fn captured_predicates_use_native_parsed_node_shapes() -> Result<()> {
        let simple_case = capture("CASE 2 WHEN 1 THEN 'one' ELSE 'other' END")?;
        assert!(matches!(
            simple_case.kind,
            StoredExpressionKind::Case { ref checks, .. }
                if matches!(checks[0].when_expression.kind, StoredExpressionKind::Comparison { kind: StoredComparison::Equal, .. })
        ));
        assert!(matches!(
            capture("NULL IS NULL")?.kind,
            StoredExpressionKind::Operator {
                kind: StoredOperator::IsNull,
                ..
            }
        ));
        assert!(matches!(
            capture("2 BETWEEN 1 AND 3")?.kind,
            StoredExpressionKind::Between { .. }
        ));
        assert!(matches!(
            capture("2 NOT BETWEEN 1 AND 3")?.kind,
            StoredExpressionKind::Operator {
                kind: StoredOperator::Not,
                ref children,
            } if matches!(children[0].kind, StoredExpressionKind::Between { .. })
        ));
        assert!(matches!(
            capture("2 IN (1, 2)")?.kind,
            StoredExpressionKind::Operator {
                kind: StoredOperator::In,
                ..
            }
        ));
        assert!(matches!(
            capture("2 NOT IN (1, 3)")?.kind,
            StoredExpressionKind::Operator {
                kind: StoredOperator::Not,
                ref children,
            } if matches!(children[0].kind, StoredExpressionKind::Operator { kind: StoredOperator::In, .. })
        ));
        assert!(matches!(
            capture("'duck' LIKE 'd%'")?.kind,
            StoredExpressionKind::Function {
                ref name,
                is_operator: true,
                ..
            } if name == &["~~"]
        ));
        Ok(())
    }

    #[test]
    fn captured_operators_bind_through_selected_operator_services_without_reparsing() -> Result<()>
    {
        let query = QueryContext::background();
        let catalog = Snapshot::new(query.type_registry());
        let casts = crate::common::cast::CastRegistry::builtins();
        let operators = OperatorRegistry::builtins();
        let functions = FunctionRegistry::builtins();
        let expressions = ScalarEvaluator;
        let context = BindContext {
            catalog: &catalog,
            casts: &casts,
            operators: &operators,
            query: &query,
            functions: &functions,
            expressions: &expressions,
            parameters: &[],
        };
        let stored = SqlBinder.capture_stored_expression(&parsed("1 + 2")?, &context)?;
        let bound = SqlBinder.bind_stored_expression(&stored, &context)?;
        assert_eq!(
            expressions.evaluate(&bound, &Vec::new(), &query)?,
            Value::Integer(3)
        );

        let mut malformed = stored;
        let StoredExpressionKind::Function { arguments, .. } = &mut malformed.kind else {
            unreachable!()
        };
        arguments.clear();
        assert!(matches!(
            SqlBinder.bind_stored_expression(&malformed, &context),
            Err(Error::Bind(_))
        ));
        Ok(())
    }
}
