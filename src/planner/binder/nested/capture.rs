//! Syntax-only nested capture. The caller owns whole-root node/identifier
//! budgets, cancellation, type-name resolution and dependency rejection. This
//! helper neither binds adapters nor evaluates children. It is test-only until
//! the shared catalog capture caller supplies those contracts.
use crate::{
    catalog::expression::{
        StoredArgument, StoredArgumentStyle, StoredExpression, StoredExpressionKind, StoredOperator,
    },
    common::{DataType, Error, Result, Value},
    parser::ast,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::planner::binder) fn capture(
    expression: &ast::Expr,
    recurse: &mut dyn FnMut(&ast::Expr) -> Result<StoredExpression>,
) -> Result<Option<StoredExpression>> {
    let captured = match expression {
        ast::Expr::Array(array) => {
            bounded(array.elem.len())?;
            let children = array.elem.iter().map(recurse).collect::<Result<_>>()?;
            if array.named {
                operator(StoredOperator::ListConstructor, children)
            } else {
                call("list_value", children)
            }
        }
        ast::Expr::Tuple(values) => {
            bounded(values.len())?;
            call("row", values.iter().map(recurse).collect::<Result<_>>()?)
        }
        ast::Expr::Dictionary(fields) => {
            bounded(fields.len())?;
            let arguments = fields
                .iter()
                .map(|field| {
                    let mut expression = recurse(&field.value)?;
                    let name = field.key.value.clone();
                    // Both properties are independently serialized by modern C++.
                    expression.alias = Some(name.clone());
                    Ok(StoredArgument {
                        name: Some(name),
                        expression,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            StoredExpression {
                alias: None,
                kind: StoredExpressionKind::Function {
                    name: vec!["struct_pack".into()],
                    arguments,
                    is_operator: false,
                    argument_style: StoredArgumentStyle::Named,
                },
            }
        }
        ast::Expr::Map(map) => {
            bounded(
                map.entries
                    .len()
                    .checked_mul(2)
                    .and_then(|n| n.checked_add(2))
                    .ok_or_else(|| {
                        Error::Resource("stored MAP capture cardinality overflow".into())
                    })?,
            )?;
            let mut keys = Vec::new();
            let mut values = Vec::new();
            for entry in &map.entries {
                keys.push(recurse(&entry.key)?);
                values.push(recurse(&entry.value)?);
            }
            call(
                "map",
                vec![call("list_value", keys), call("list_value", values)],
            )
        }
        ast::Expr::CompoundFieldAccess { root, access_chain } => {
            // A flat parser chain becomes a recursively owned stored tree.
            // Bound this expansion before construction, not only afterward in
            // the caller's whole-root validation (or during recursive drop).
            if access_chain.len() > 64 {
                return Err(Error::Resource("stored nested capture path depth".into()));
            }
            // C++ retains t.s.field as one ColumnRef until binding, whereas
            // sqlparser can put those dots in this access chain. Closed defaults
            // cannot depend on that base; do not invent a qualifier/field split.
            if matches!(
                root.as_ref(),
                ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_)
            ) {
                return Err(Error::Unsupported(
                    "row-dependent nested capture base".into(),
                ));
            }
            let mut value = recurse(root)?;
            for access in access_chain {
                let (kind, key) = match access {
                    ast::AccessExpr::Dot(ast::Expr::Identifier(name)) => (
                        StoredOperator::Field,
                        StoredExpression::literal(
                            DataType::Varchar,
                            Value::Varchar(name.value.clone()),
                        ),
                    ),
                    ast::AccessExpr::Subscript(ast::Subscript::Index { index }) => {
                        (StoredOperator::Index, recurse(index)?)
                    }
                    _ => {
                        return Err(Error::Unsupported(
                            "stored nested slice or accessor capture".into(),
                        ));
                    }
                };
                value = operator(kind, vec![value, key]);
            }
            value
        }
        _ => return Ok(None),
    };
    Ok(Some(captured))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bounded(children: usize) -> Result<()> {
    if children >= 16_384 {
        return Err(Error::Resource("stored nested capture child limit".into()));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn call(name: &str, children: Vec<StoredExpression>) -> StoredExpression {
    StoredExpression {
        alias: None,
        kind: StoredExpressionKind::Function {
            name: vec![name.into()],
            arguments: children
                .into_iter()
                .map(|expression| StoredArgument {
                    name: None,
                    expression,
                })
                .collect(),
            is_operator: false,
            argument_style: StoredArgumentStyle::Named,
        },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn operator(kind: StoredOperator, children: Vec<StoredExpression>) -> StoredExpression {
    StoredExpression {
        alias: None,
        kind: StoredExpressionKind::Operator { kind, children },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{DuckDbParser, Parser, Statement};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn expression(sql: &str) -> Result<ast::Expr> {
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
        let ast::SelectItem::UnnamedExpr(expression) =
            select.projection.into_iter().next().unwrap()
        else {
            unreachable!()
        };
        Ok(expression)
    }

    // A deliberately small fake parent capture service, not a second binder.
    // It keeps function and cast nodes and does not execute their children.
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn retained(expression: &ast::Expr) -> Result<StoredExpression> {
        if let ast::Expr::Nested(expression) = expression {
            return retained(expression);
        }
        if let Some(expression) = capture(expression, &mut retained)? {
            return Ok(expression);
        }
        match expression {
            ast::Expr::Value(value) => match &value.value {
                ast::Value::Null => Ok(StoredExpression::literal(DataType::Null, Value::Null)),
                ast::Value::Number(value, _) => Ok(StoredExpression::literal(
                    DataType::Integer,
                    Value::Integer(value.parse().unwrap()),
                )),
                ast::Value::SingleQuotedString(value) => Ok(StoredExpression::literal(
                    DataType::Varchar,
                    Value::Varchar(value.clone()),
                )),
                _ => Err(Error::Unsupported("test capture literal".into())),
            },
            ast::Expr::Cast {
                expr,
                data_type: ast::DataType::SmallInt(_),
                ..
            } => Ok(StoredExpression {
                alias: None,
                kind: StoredExpressionKind::Cast {
                    expression: Box::new(retained(expr)?),
                    target: DataType::SmallInt,
                    try_cast: false,
                },
            }),
            ast::Expr::Function(function) => {
                let arguments = super::super::scalar_arguments(function)?;
                let arguments = arguments
                    .expressions
                    .iter()
                    .enumerate()
                    .map(|(index, expression)| {
                        let mut expression = retained(expression)?;
                        expression.alias = arguments.aliases[index].clone();
                        Ok(StoredArgument {
                            name: arguments.names[index].clone(),
                            expression,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(StoredExpression {
                    alias: None,
                    kind: StoredExpressionKind::Function {
                        name: function
                            .name
                            .0
                            .iter()
                            .map(|part| part.as_ident().unwrap().value.clone())
                            .collect(),
                        arguments,
                        is_operator: false,
                        argument_style: StoredArgumentStyle::Named,
                    },
                })
            }
            _ => Err(Error::Unsupported(
                "test parent capture dependency or syntax".into(),
            )),
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn nested_capture_retains_constructor_identity_names_typed_null_casts_and_access_order()
    -> Result<()> {
        let bracket = retained(&expression("[1,NULL::SMALLINT]")?)?;
        assert!(
            matches!(bracket.kind, StoredExpressionKind::Function { ref name, .. } if name == &["list_value"])
        );
        let array = retained(&expression("ARRAY[1,NULL::SMALLINT]")?)?;
        assert!(matches!(
            array.kind,
            StoredExpressionKind::Operator {
                kind: StoredOperator::ListConstructor,
                ..
            }
        ));
        let structure = retained(&expression("{'Quoted.Name':NULL::SMALLINT}")?)?;
        let StoredExpressionKind::Function {
            name,
            arguments,
            argument_style,
            ..
        } = structure.kind
        else {
            unreachable!()
        };
        assert_eq!(name, ["struct_pack"]);
        assert_eq!(argument_style, StoredArgumentStyle::Named);
        assert_eq!(arguments[0].name.as_deref(), Some("Quoted.Name"));
        assert_eq!(
            arguments[0].expression.alias.as_deref(),
            Some("Quoted.Name")
        );
        assert!(matches!(
            arguments[0].expression.kind,
            StoredExpressionKind::Cast {
                target: DataType::SmallInt,
                ..
            }
        ));
        let path = retained(&expression("(struct_pack(Items:=[1,NULL])).Items[2]")?)?;
        let StoredExpressionKind::Operator { kind, children } = path.kind else {
            unreachable!()
        };
        assert_eq!(kind, StoredOperator::Index);
        assert!(matches!(
            children[0].kind,
            StoredExpressionKind::Operator {
                kind: StoredOperator::Field,
                ..
            }
        ));
        let map = retained(&expression("MAP {'first':1,'second':NULL}")?)?;
        let StoredExpressionKind::Function {
            name, arguments, ..
        } = map.kind
        else {
            unreachable!()
        };
        assert_eq!(name, ["map"]);
        assert_eq!(arguments.len(), 2);
        for argument in arguments {
            assert!(
                matches!(argument.expression.kind, StoredExpressionKind::Function { ref name, .. } if name == &["list_value"])
            );
        }
        let value = retained(&expression("{'Bad':fail('not executed')}")?)?;
        let StoredExpressionKind::Function { arguments, .. } = value.kind else {
            unreachable!()
        };
        assert!(
            matches!(arguments[0].expression.kind, StoredExpressionKind::Function { ref name, .. } if name == &["fail"])
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn nested_capture_rejects_qualified_dependencies_slices_and_unbounded_local_children()
    -> Result<()> {
        for sql in [
            "t.s.Items[2]",
            "\"t.q\".\"s.x\".\"Items.y\"[2]",
            "(t).s[1]",
            "[1,2][1:2]",
        ] {
            assert!(
                matches!(retained(&expression(sql)?), Err(Error::Unsupported(_))),
                "{sql}"
            );
        }
        let array = ast::Expr::Array(ast::Array {
            elem: vec![expression("1")?; 16_384],
            named: false,
        });
        assert!(matches!(
            capture(&array, &mut |_| panic!(
                "local arity must fail before callbacks"
            )),
            Err(Error::Resource(_))
        ));
        let path = ast::Expr::CompoundFieldAccess {
            root: Box::new(expression("[1]")?),
            access_chain: vec![
                ast::AccessExpr::Subscript(ast::Subscript::Index {
                    index: expression("1")?
                });
                65
            ],
        };
        assert!(matches!(
            capture(&path, &mut |_| panic!(
                "path depth must fail before callbacks"
            )),
            Err(Error::Resource(_))
        ));
        assert!(
            capture(&expression("1")?, &mut |_| panic!(
                "nonfamily syntax must not recurse"
            ))?
            .is_none()
        );
        let array = expression("[1,2]")?;
        assert!(matches!(
            capture(&array, &mut |_| Err(Error::Interrupted)),
            Err(Error::Interrupted)
        ));
        assert!(matches!(
            capture(&array, &mut |_| Err(Error::Resource(
                "selected callback".into()
            ))),
            Err(Error::Resource(_))
        ));
        Ok(())
    }
}
