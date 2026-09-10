mod alter;
mod coercion;
mod expression;
mod grouping;
mod nested;
mod query;
mod recursive;
mod scope;
mod settings;
mod statement;
mod subquery;
mod table;
mod window;

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
};

use super::{
    BindContext, Binder, BoundExpr, BoundStatement, ExprKind, Field, LogicalPlan, PlanNode, Schema,
    aggregation::{AggregateOutput, Aggregation, GroupingSet},
    expression::{BinaryOp, UnaryOp},
    logical::{AggregateExpr, JoinKind, OrderExpr},
};
use crate::{
    catalog::{ColumnDefinition, TableDefinition, TableName},
    common::{DataType, Error, Result, Value, cast::CastMode},
    function::operator::{Operator, OperatorArgument},
    parser::ast,
};
use scope::{Relation, Scope, SelectItem};

#[derive(Default)]
pub struct SqlBinder;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Binder for SqlBinder {
    fn name(&self) -> &'static str {
        "sql-binder"
    }
    fn bind(
        &self,
        statement: &crate::parser::Statement,
        context: &BindContext<'_>,
    ) -> Result<BoundStatement> {
        context.query.check()?;
        let mut state = State {
            context,
            parameters_allowed: true,
            ctes: BTreeMap::new(),
            outer: Vec::new(),
        };
        match statement {
            crate::parser::Statement::Sql(statement) => state.statement(statement),
            crate::parser::Statement::Checkpoint => Ok(BoundStatement::Checkpoint),
            crate::parser::Statement::ResetSetting { name, scope } => {
                state.setting(name, *scope, None)
            }
        }
    }
}

struct State<'a, 'b> {
    context: &'a BindContext<'b>,
    parameters_allowed: bool,
    ctes: BTreeMap<String, CommonTable>,
    outer: Vec<CorrelationScope>,
}

#[derive(Clone)]
struct CommonTable {
    plan: LogicalPlan,
    depth: usize,
}

#[derive(Clone)]
struct CorrelationScope {
    fields: Scope,
    /// Source column to grouped output position; None is an illegal ungrouped reference.
    columns: Vec<Option<usize>>,
}

struct GroupScope {
    groups: Vec<(ast::Expr, BoundExpr)>,
    aliases: BTreeMap<String, usize>,
    outputs: RefCell<Vec<(ast::Expr, AggregateOutput)>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unsupported(thing: impl std::fmt::Display) -> Error {
    Error::Unsupported(thing.to_string())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn table_name(name: &ast::ObjectName) -> Result<TableName> {
    let parts = name
        .0
        .iter()
        .map(|p| {
            p.as_ident()
                .map(|i| i.value.clone())
                .ok_or_else(|| unsupported(name))
        })
        .collect::<Result<Vec<_>>>()?;
    match parts.as_slice() {
        [table] => Ok(TableName::main(table)),
        [schema, table] => Ok(TableName::new(schema, table)),
        _ => Err(unsupported("cross-database names")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    fn data_type(&self, data_type: &ast::DataType) -> Result<DataType> {
        use ast::DataType as T;
        let resolved = match data_type {
            T::Boolean | T::Bool => Ok(DataType::Boolean),
            T::Date => Ok(DataType::Date),
            T::Enum(members, None) if members.is_empty() => Err(Error::Bind(
                "ENUM type requires at least one argument".into(),
            )),
            T::Enum(members, None) => DataType::enumeration(
                members
                    .iter()
                    .map(|member| match member {
                        ast::EnumMember::Name(label) => Ok(label.clone()),
                        _ => Err(Error::Bind(
                            "ENUM labels cannot specify ordinal assignments".into(),
                        )),
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            T::Time(Some(_), _) => Err(Error::Bind("TIME does not allow type modifiers".into())),
            T::Time(None, zone) => Ok(
                if matches!(
                    zone,
                    ast::TimezoneInfo::WithTimeZone | ast::TimezoneInfo::Tz
                ) {
                    DataType::TimeTz
                } else {
                    DataType::Time
                },
            ),
            T::Timestamp(Some(_), ast::TimezoneInfo::Tz) => Err(Error::Bind(
                "TIMESTAMPTZ does not allow type modifiers".into(),
            )),
            T::Timestamp(Some(precision), _) => match precision {
                0 => Ok(DataType::TimestampS),
                1..=3 => Ok(DataType::TimestampMs),
                4..=6 => Ok(DataType::Timestamp),
                // Pinned development accepts precision10 and resolves an
                // explicit modifier before WITH TIME ZONE. Preserve both.
                7..=10 => Ok(DataType::TimestampNs),
                _ => Err(Error::Bind("TIMESTAMP precision outside 0..=10".into())),
            },
            T::Timestamp(None, zone) => Ok(
                if matches!(
                    zone,
                    ast::TimezoneInfo::WithTimeZone | ast::TimezoneInfo::Tz
                ) {
                    DataType::TimestampTz
                } else {
                    DataType::Timestamp
                },
            ),
            T::Interval { .. } => Ok(DataType::Interval),
            T::Blob(None) | T::Binary(None) | T::Varbinary(None) | T::Bytea => Ok(DataType::Blob),
            T::Blob(Some(_)) | T::Binary(Some(_)) | T::Varbinary(Some(_)) => {
                Err(Error::Bind("BLOB does not take type parameters".into()))
            }
            T::Uuid => Ok(DataType::Uuid),
            T::Array(element) => {
                use ast::ArrayElemTypeDef as A;
                match element {
                    A::SquareBracket(element, Some(length)) => {
                        Ok(crate::common::NestedType::Array {
                            element: self.data_type(element)?,
                            length: usize::try_from(*length)
                                .map_err(|_| Error::Bind("ARRAY size out of range".into()))?,
                        }
                        .data_type())
                    }
                    A::SquareBracket(element, None)
                    | A::AngleBracket(element)
                    | A::Parenthesis(element) => {
                        Ok(crate::common::NestedType::List(self.data_type(element)?).data_type())
                    }
                    A::None => Err(Error::Bind("ARRAY requires an element type".into())),
                }
            }
            T::Struct(fields, _) => Ok(crate::common::NestedType::Struct(
                fields
                    .iter()
                    .map(|field| {
                        Ok((
                            field
                                .field_name
                                .as_ref()
                                .ok_or_else(|| Error::Bind("STRUCT field requires a name".into()))?
                                .value
                                .clone(),
                            self.data_type(&field.field_type)?,
                        ))
                    })
                    .collect::<Result<_>>()?,
            )
            .data_type()),
            T::Map(key, value) => Ok(crate::common::NestedType::Map {
                key: self.data_type(key)?,
                value: self.data_type(value)?,
            }
            .data_type()),
            T::Union(fields) => Ok(crate::common::NestedType::Union(
                fields
                    .iter()
                    .map(|field| {
                        Ok((
                            field.field_name.value.clone(),
                            self.data_type(&field.field_type)?,
                        ))
                    })
                    .collect::<Result<_>>()?,
            )
            .data_type()),
            T::TinyInt(_) => Ok(DataType::TinyInt),
            T::SmallInt(_) | T::Int2(_) | T::Int16 => Ok(DataType::SmallInt),
            T::Int(_) | T::Integer(_) | T::Int4(_) | T::Int32 => Ok(DataType::Integer),
            T::BigInt(_) | T::Int8(_) | T::Int64 => Ok(DataType::BigInt),
            T::HugeInt | T::Int128 => Ok(DataType::HugeInt),
            T::UTinyInt | T::UInt8 => Ok(DataType::UTinyInt),
            T::USmallInt | T::UInt16 => Ok(DataType::USmallInt),
            T::UInt32 => Ok(DataType::UInteger),
            T::UBigInt | T::UInt64 => Ok(DataType::UBigInt),
            T::UHugeInt | T::UInt128 => Ok(DataType::UHugeInt),
            T::Numeric(info) | T::Decimal(info) | T::Dec(info) => {
                let (width, scale) = match info {
                    ast::ExactNumberInfo::None => (18, 3),
                    ast::ExactNumberInfo::Precision(width) => (*width, 0),
                    ast::ExactNumberInfo::PrecisionAndScale(width, scale) => (*width, *scale),
                };
                if !(1..=38).contains(&width) || scale < 0 || scale as u64 > width {
                    return Err(Error::Bind(
                        "DECIMAL requires width 1..38 and scale 0..width".into(),
                    ));
                }
                Ok(DataType::Decimal {
                    width: width as u8,
                    scale: scale as u8,
                })
            }
            T::Double(_) | T::DoublePrecision | T::Float8 | T::Float64 => Ok(DataType::Double),
            T::Float(ast::ExactNumberInfo::None | ast::ExactNumberInfo::Precision(1..=24))
            | T::Real
            | T::Float4
            | T::Float32 => Ok(DataType::Float),
            T::Float(ast::ExactNumberInfo::Precision(25..=53)) => Ok(DataType::Double),
            T::Float(_) => Err(Error::Bind(
                "FLOAT precision must be between 1 and 53 bits".into(),
            )),
            T::Varchar(_)
            | T::Char(_)
            | T::Character(_)
            | T::CharacterVarying(_)
            | T::CharVarying(_)
            | T::Text
            | T::String(_) => Ok(DataType::Varchar),
            T::Custom(name, modifiers) => {
                let name = name
                    .0
                    .iter()
                    .map(|p| {
                        p.as_ident()
                            .map(|i| i.value.to_ascii_lowercase())
                            .ok_or_else(|| unsupported("type name expression"))
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(".");
                if name == "uinteger" && modifiers.is_empty() {
                    self.context.query.types().bind(&DataType::UInteger)?;
                    return Ok(DataType::UInteger);
                }
                if modifiers.is_empty() {
                    let temporal = match name.as_str() {
                        "time_ns" => Some(DataType::TimeNs),
                        "timestamp_s" => Some(DataType::TimestampS),
                        "timestamp_ms" => Some(DataType::TimestampMs),
                        "timestamp_ns" => Some(DataType::TimestampNs),
                        "timestamptz_ns" => Some(DataType::TimestampTzNs),
                        _ => None,
                    };
                    if let Some(data_type) = temporal {
                        self.context.query.types().bind(&data_type)?;
                        return Ok(data_type);
                    }
                }
                if name == "guid" && modifiers.is_empty() {
                    self.context.query.types().bind(&DataType::Uuid)?;
                    return Ok(DataType::Uuid);
                }
                if name == "variant" && modifiers.is_empty() {
                    return Ok(crate::common::NestedType::Variant.data_type());
                }
                let parameters = modifiers
                    .iter()
                    .map(|p| {
                        if let Ok(value) = p.parse::<i64>() {
                            Ok(crate::common::TypeParameter::Integer(value))
                        } else if let Some(text) =
                            p.strip_prefix('\'').and_then(|t| t.strip_suffix('\''))
                        {
                            Ok(crate::common::TypeParameter::Text(text.replace("''", "'")))
                        } else {
                            Err(unsupported(
                                "type parameters must be integer or string literals",
                            ))
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(DataType::extension(name, parameters))
            }
            _ => Err(unsupported(format!("type {data_type}"))),
        }?;
        self.context.query.types().bind(&resolved)?;
        Ok(resolved)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    fn literal(&self, expr: &ast::Expr) -> Result<Value> {
        match expr {
            ast::Expr::Value(value) => match &value.value {
                ast::Value::Null => Ok(Value::Null),
                ast::Value::Boolean(v) => Ok(Value::Boolean(*v)),
                ast::Value::SingleQuotedString(v) | ast::Value::EscapedStringLiteral(v) => {
                    Ok(Value::Varchar(v.clone()))
                }
                ast::Value::Number(v, _) => number(v),
                _ => Err(unsupported(expr)),
            },
            ast::Expr::UnaryOp {
                op: ast::UnaryOperator::Minus,
                expr,
            } if matches!(expr.as_ref(), ast::Expr::Value(v) if matches!(&v.value, ast::Value::Number(_, _))) =>
            {
                let ast::Expr::Value(v) = expr.as_ref() else {
                    unreachable!()
                };
                let ast::Value::Number(v, _) = &v.value else {
                    unreachable!()
                };
                number(&format!("-{v}"))
            }
            _ => {
                let bound = self.expr(expr, &Scope::default(), None)?;
                if !constant_expression(&bound) {
                    return Err(unsupported(format!(
                        "a constant expression is required here: {expr}"
                    )));
                }
                let value =
                    self.context
                        .expressions
                        .evaluate(&bound, &Vec::new(), self.context.query)?;
                self.context
                    .query
                    .types()
                    .bind(&bound.data_type)?
                    .validate(&value, self.context.query)
                    .map_err(|error| match error {
                        Error::Conversion(_) => {
                            Error::Internal("constant evaluator returned an invalid value".into())
                        }
                        other => other,
                    })?;
                Ok(value)
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn constant_expression(expression: &BoundExpr) -> bool {
    match &expression.kind {
        ExprKind::Literal(_) => true,
        ExprKind::Cast(inner, ..) | ExprKind::Unary(_, inner) => constant_expression(inner),
        ExprKind::Binary(_, left, right, _) => {
            constant_expression(left) && constant_expression(right)
        }
        ExprKind::Operator(function, arguments) => {
            let effects = function.effects();
            !effects.volatile
                && !effects.external_access
                && arguments.iter().all(constant_expression)
        }
        ExprKind::Scalar(function, arguments) => {
            let effects = function.effects();
            !effects.volatile
                && !effects.external_access
                && arguments.iter().all(constant_expression)
        }
        _ => false,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn schema(table: &TableDefinition) -> Schema {
    table
        .columns
        .iter()
        .map(|c| Field {
            qualifier: Some(table.name.name.clone()),
            name: c.name.clone(),
            data_type: c.data_type.clone(),
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn resolve(schema: &[Field], parts: &[String]) -> Result<usize> {
    resolve_optional(schema, parts)?
        .ok_or_else(|| Error::Bind(format!("column {} not found", parts.join("."))))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn resolve_optional(schema: &[Field], parts: &[String]) -> Result<Option<usize>> {
    let name = parts
        .last()
        .ok_or_else(|| Error::Bind("empty column name".into()))?;
    let qualifier = if parts.len() == 2 {
        parts.first()
    } else {
        None
    };
    if parts.len() > 2 {
        return Err(unsupported("column names with more than two parts"));
    }
    let matches: Vec<_> = schema
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.name.eq_ignore_ascii_case(name)
                && qualifier.is_none_or(|q| {
                    f.qualifier
                        .as_ref()
                        .is_some_and(|v| v.eq_ignore_ascii_case(q))
                })
        })
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [index] => Ok(Some(*index)),
        [] => Ok(None),
        _ => Err(Error::Bind(format!("ambiguous column {}", parts.join(".")))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    fn boolean(&self, expr: BoundExpr) -> Result<BoundExpr> {
        if !matches!(expr.data_type, DataType::Boolean | DataType::Null) {
            return Err(Error::Bind("predicate must be BOOLEAN".into()));
        }
        expr.cast(
            DataType::Boolean,
            CastMode::Implicit,
            self.context.casts,
            self.context.query.types(),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    fn operator(&self, operator: Operator, arguments: Vec<BoundExpr>) -> Result<BoundExpr> {
        let inputs: Vec<_> = arguments
            .iter()
            .map(|arg| OperatorArgument {
                data_type: &arg.data_type,
                integer_literal: match arg.kind {
                    ExprKind::Literal(Value::Integer(n)) if arg.data_type == DataType::Integer => {
                        Some(n)
                    }
                    _ => None,
                },
            })
            .collect();
        let resolved = self.context.operators.resolve(
            operator,
            &inputs,
            self.context.casts,
            self.context.query.types(),
            self.context.query,
        )?;
        let signature = resolved.function.signature();
        let arguments = arguments
            .into_iter()
            .zip(&signature.arguments)
            .zip(resolved.coercions)
            .map(|((value, target), mode)| {
                value.cast(
                    target.clone(),
                    mode,
                    self.context.casts,
                    self.context.query.types(),
                )
            })
            .collect::<Result<_>>()?;
        Ok(BoundExpr {
            data_type: signature.result.clone(),
            kind: ExprKind::Operator(resolved.function, arguments),
        })
    }

    fn sql_literal(&self, expr: &ast::Expr) -> Result<BoundExpr> {
        let mut value = BoundExpr::literal(self.literal(expr)?);
        if matches!(value.kind, ExprKind::Literal(Value::Integer(n)) if i32::try_from(n).is_ok()) {
            value.data_type = DataType::Integer;
        }
        Ok(value)
    }

    fn binary(&self, op: BinaryOp, left: BoundExpr, right: BoundExpr) -> Result<BoundExpr> {
        use BinaryOp::*;
        let (left, right, data_type) = match op {
            And | Or => (self.boolean(left)?, self.boolean(right)?, DataType::Boolean),
            _ => {
                let target = self.comparison_type(
                    &left.data_type,
                    coercion::string_literal(&left),
                    &right.data_type,
                    coercion::string_literal(&right),
                    matches!(op, Equal | NotEqual),
                )?;
                (
                    self.comparison_cast(left, &target)?,
                    self.comparison_cast(right, &target)?,
                    DataType::Boolean,
                )
            }
        };
        let operand_type = self.context.query.types().bind(&left.data_type)?;
        Ok(BoundExpr {
            data_type,
            kind: ExprKind::Binary(op, Box::new(left), Box::new(right), operand_type.into()),
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn function_arg(arg: &ast::FunctionArg) -> Result<ast::Expr> {
    match arg {
        ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(e)) => Ok(e.clone()),
        _ => Err(unsupported(arg)),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn function_arguments(function: &ast::Function) -> Result<Vec<ast::Expr>> {
    if !matches!(function.parameters, ast::FunctionArguments::None) {
        return Err(unsupported("parametric function"));
    }
    match &function.args {
        ast::FunctionArguments::List(list) if list.clauses.is_empty() => {
            if list.args.len() == 1
                && matches!(
                    list.args[0],
                    ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard)
                )
            {
                if function.name.to_string().eq_ignore_ascii_case("count") {
                    return Ok(Vec::new());
                }
                return Err(Error::Bind("wildcard is only valid in count(*)".into()));
            }
            list.args.iter().map(function_arg).collect()
        }
        _ => Err(unsupported("function arguments")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn alias(plan: &mut LogicalPlan, alias: &ast::TableAlias) -> Result<()> {
    if alias.columns.len() > plan.schema.len() {
        return Err(Error::Bind("too many column aliases".into()));
    }
    for (i, field) in plan.schema.iter_mut().enumerate() {
        field.qualifier = Some(alias.name.value.clone());
        if let Some(column) = alias.columns.get(i) {
            field.name = column.name.value.clone();
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn join(
    left: LogicalPlan,
    right: LogicalPlan,
    kind: JoinKind,
    condition: BoundExpr,
) -> LogicalPlan {
    let mut fields = left.schema.clone();
    if !matches!(kind, JoinKind::Semi | JoinKind::Anti) {
        fields.extend(right.schema.clone());
    }
    LogicalPlan {
        schema: fields,
        node: PlanNode::Join {
            left: Box::new(left),
            right: Box::new(right),
            kind,
            condition,
        },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    fn coerce_plan(
        &self,
        plan: LogicalPlan,
        types: &[DataType],
        mode: CastMode,
    ) -> Result<LogicalPlan> {
        let fields = plan
            .schema
            .iter()
            .zip(types)
            .map(|(f, t)| Field {
                data_type: t.clone(),
                ..f.clone()
            })
            .collect();
        let expressions = plan
            .schema
            .iter()
            .zip(types)
            .enumerate()
            .map(|(i, (f, t))| {
                BoundExpr::column(i, f.data_type.clone()).cast(
                    t.clone(),
                    mode,
                    self.context.casts,
                    self.context.query.types(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LogicalPlan {
            schema: fields,
            node: PlanNode::Projection {
                input: Box::new(plan),
                expressions,
            },
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_wildcard(options: &ast::WildcardAdditionalOptions) -> Result<()> {
    if options.opt_ilike.is_some()
        || options.opt_exclude.is_some()
        || options.opt_except.is_some()
        || options.opt_replace.is_some()
        || options.opt_rename.is_some()
    {
        Err(unsupported("wildcard modifiers"))
    } else {
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    fn nonnegative(&self, expr: &ast::Expr) -> Result<usize> {
        usize::try_from(self.literal(expr)?.as_i128()?)
            .map_err(|_| Error::Bind("LIMIT/OFFSET must be a nonnegative integer".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ordinal(expr: &ast::Expr, width: usize) -> Result<Option<usize>> {
    if let ast::Expr::Value(value) = expr
        && let ast::Value::Number(v, _) = &value.value
        && let Ok(index) = v.parse::<usize>()
    {
        if index == 0 || index > width {
            return Err(Error::Bind(format!(
                "column position {index} outside 1..={width}"
            )));
        }
        return Ok(Some(index - 1));
    }
    Ok(None)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn order_expressions(order: Option<&ast::OrderBy>, width: usize) -> Result<Vec<ast::OrderByExpr>> {
    match order {
        None => Ok(Vec::new()),
        Some(ast::OrderBy {
            kind: ast::OrderByKind::Expressions(expressions),
            interpolate: None,
        }) if expressions.iter().all(|e| e.with_fill.is_none()) => Ok(expressions.clone()),
        Some(ast::OrderBy {
            kind: ast::OrderByKind::All(options),
            interpolate: None,
        }) => Ok((1..=width)
            .map(|index| ast::OrderByExpr {
                // Bind output ordinals after wildcard expansion. Sorting must
                // reuse projected values, including volatile expressions.
                expr: ast::Expr::Value(ast::Value::Number(index.to_string(), false).into()),
                options: *options,
                with_fill: None,
            })
            .collect()),
        _ => Err(unsupported("ORDER BY modifiers")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn number(value: &str) -> Result<Value> {
    if value.contains('.') && !value.contains(['e', 'E']) {
        let unsigned = value.strip_prefix('-').unwrap_or(value);
        let width = unsigned.len() - 1;
        let scale = unsigned.len() - unsigned.find('.').unwrap() - 1;
        if width <= 38 && width > 0 {
            let (negative, magnitude) = crate::common::cast::numeric::parse_scaled(
                value,
                scale as u8,
                &crate::parallel::QueryContext::background(),
            )?;
            let n = i128::try_from(magnitude)
                .map_err(|_| Error::Conversion("decimal literal overflow".into()))?;
            return crate::common::numeric::decimal(
                if negative { -n } else { n },
                width as u8,
                scale as u8,
            );
        }
    }
    if value.contains(['.', 'e', 'E']) {
        value
            .parse()
            .map(Value::Double)
            .map_err(|_| Error::Conversion(format!("invalid number {value}")))
    } else {
        value
            .parse()
            .map(Value::Integer)
            .map_err(|_| Error::Conversion(format!("integer out of range: {value}")))
    }
}
