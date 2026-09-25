//! Combination is a binding context, not a global implicit cast. CASE, VALUES,
//! set operations and comparisons retain selected casts after common-type
//! inference. Equality has an additional type fallback ordering does not share.
use super::*;
use crate::common::type_registry::IntegerLiteral;

#[derive(Clone, Copy)]
pub(super) enum CombinationSequence {
    /// Collection templates skip later NULLs and identical pseudo-types.
    Collection,
    /// CASE normalizes each pair, including equal literals and later NULLs.
    Case,
    /// A selected scalar requests left-to-right pair normalization.
    Ordered,
    /// VALUES starts from SQL NULL and normalizes every row's contribution.
    Values,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn string_literal(value: &BoundExpr) -> bool {
    matches!(&value.kind, ExprKind::Literal(Value::Varchar(_)))
        && value.data_type == DataType::Varchar
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn integer_literal(value: &BoundExpr) -> Option<i128> {
    match full_integer_literal(value) {
        Some(IntegerLiteral::Signed(value)) => Some(value),
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn full_integer_literal(value: &BoundExpr) -> Option<IntegerLiteral> {
    match &value.kind {
        ExprKind::Literal(Value::Integer(number))
            if value.data_type.integer_bits().is_some()
                && Value::Integer(*number).fits_type(&value.data_type) =>
        {
            Some(IntegerLiteral::Signed(*number))
        }
        ExprKind::Literal(Value::Unsigned(number))
            if value.data_type.is_unsigned_integer()
                && Value::Unsigned(*number).fits_type(&value.data_type) =>
        {
            Some(IntegerLiteral::Unsigned(*number))
        }
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn integer_literal_fits(value: &BoundExpr, target: &DataType) -> bool {
    integer_literal(value).is_some_and(|number| {
        if target.is_unsigned_integer() {
            number >= 0 && Value::Unsigned(number as u128).fits_type(target)
        } else {
            target.integer_bits().is_some() && Value::Integer(number).fits_type(target)
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn scalar_argument_cast_mode(
    expression: &BoundExpr,
    target: &DataType,
    selected: CastMode,
) -> CastMode {
    if selected == CastMode::Implicit
        && (string_literal(expression) || integer_literal_fits(expression, target))
    {
        CastMode::Explicit
    } else {
        selected
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn ordered_combination_type<'b>(
        &self,
        arguments: impl IntoIterator<Item = &'b BoundExpr>,
        context: CombinationSequence,
    ) -> Result<DataType> {
        ordered_combination_type(self.context, arguments, context)
    }

    pub(super) fn combination_cast_mode(
        &self,
        source: &DataType,
        target: &DataType,
    ) -> Result<CastMode> {
        combination_cast_mode(self.context, source, target)
    }
    pub(super) fn combination_cast(
        &self,
        value: BoundExpr,
        target: &DataType,
    ) -> Result<BoundExpr> {
        let mode = self.combination_cast_mode(&value.data_type, target)?;
        value.cast(
            target.clone(),
            mode,
            self.context.casts,
            self.context.query.types(),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn ordered_combination_type<'b>(
    binding: &BindContext<'_>,
    arguments: impl IntoIterator<Item = &'b BoundExpr>,
    context: CombinationSequence,
) -> Result<DataType> {
    binding.query.check()?;
    let mut arguments = arguments.into_iter();
    let Some(first) = arguments.next() else {
        return Ok(DataType::Null);
    };
    let types = binding.query.types();
    let mut child = first.data_type.clone();
    let mut literal = string_literal(first);
    let mut integer = full_integer_literal(first);
    for argument in arguments {
        binding.query.check()?;
        let other_literal = string_literal(argument);
        let other_integer = full_integer_literal(argument);
        if matches!(context, CombinationSequence::Collection) {
            // These are collection-template rules, not generic CASE
            // combination. CASE must normalize even equal pseudo-types.
            if argument.data_type == DataType::Null
                || (literal && other_literal)
                || (integer.is_some() && integer == other_integer && child == argument.data_type)
            {
                continue;
            }
        }
        let inferred = types.try_common_type_with_literals(
            &child,
            &argument.data_type,
            integer,
            other_integer,
        )?;
        child = if let Some(inferred) = inferred {
            inferred
        } else if literal {
            argument.data_type.clone()
        } else if other_literal && child != DataType::Null {
            child
        } else {
            if matches!(context, CombinationSequence::Values) {
                // ExpressionListRef calls MaxLogicalType, whose recognized
                // no-common-type result is NotImplementedException. Selected
                // adapter failures above are not caught or recategorized.
                let name = |ty: &DataType, hint: Option<IntegerLiteral>| {
                    if hint.is_some() {
                        "INTEGER_LITERAL".to_owned()
                    } else {
                        ty.to_string()
                    }
                };
                return Err(Error::NotImplemented(format!(
                    "Cannot combine types {} and {} - an explicit cast is required",
                    name(&child, integer),
                    name(&argument.data_type, other_integer)
                )));
            }
            let context = match context {
                CombinationSequence::Case => "CASE expression",
                CombinationSequence::Collection => "sequence children",
                CombinationSequence::Ordered => "function arguments",
                CombinationSequence::Values => "VALUES column",
            };
            return Err(Error::Bind(format!(
                "Cannot combine {context} of type {child} and {}",
                argument.data_type
            )));
        };
        literal = false;
        integer = None;
    }
    Ok(child)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn combination_cast_mode(
    binding: &BindContext<'_>,
    source: &DataType,
    target: &DataType,
) -> Result<CastMode> {
    Ok(
        if binding
            .casts
            .coercion_cost_with_types(source, target, CastMode::Implicit, binding.query.types())?
            .is_some()
        {
            CastMode::Implicit
        } else {
            CastMode::Explicit
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn comparison_type(
        &self,
        left: &DataType,
        left_literal: bool,
        right: &DataType,
        right_literal: bool,
        equality: bool,
    ) -> Result<DataType> {
        let types = self.context.query.types();
        let mut common = types.try_common_type(left, right)?;
        if common.is_none() {
            common = match (left_literal, right_literal) {
                (true, false) => Some(right.clone()),
                (false, true) => Some(left.clone()),
                _ => None,
            };
        }
        if common.is_none() {
            let a = self.context.casts.coercion_cost_with_types(
                left,
                right,
                CastMode::Implicit,
                types,
            )?;
            let b = self.context.casts.coercion_cost_with_types(
                right,
                left,
                CastMode::Implicit,
                types,
            )?;
            common = match (a, b) {
                (Some(a), Some(b)) if a < b => Some(right.clone()),
                (_, Some(_)) => Some(left.clone()),
                (Some(_), None) => Some(right.clone()),
                _ if *left == DataType::Boolean && right.is_integer() => Some(right.clone()),
                _ if *right == DataType::Boolean && left.is_integer() => Some(left.clone()),
                _ => None,
            };
        }
        if common.is_none() && equality {
            // ENUM-to-other comparisons use labels. Same/different dictionaries
            // were already resolved by their selected type adapter above.
            let a = if matches!(left, DataType::Enum(_)) {
                &DataType::Varchar
            } else {
                left
            };
            let b = if matches!(right, DataType::Enum(_)) {
                &DataType::Varchar
            } else {
                right
            };
            if let (Some(a_score), Some(b_score)) = (equality_score(a), equality_score(b)) {
                common = Some(if a_score < b_score {
                    b.clone()
                } else {
                    a.clone()
                });
            }
        }
        let mut common = common.ok_or_else(|| Error::Bind(format!(
            "Cannot compare values of type {left} and type {right} - an explicit cast is required"
        )))?;
        if common == DataType::Varchar {
            if prefer_over_string(left) {
                common = left.clone();
            } else if prefer_over_string(right) {
                common = right.clone();
            }
        }
        types.bind(&common)?;
        Ok(common)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn prefer_over_string(ty: &DataType) -> bool {
    ty.is_numeric()
        || *ty == DataType::Boolean
        || *ty == DataType::Date
        || (ty.is_temporal() && *ty != DataType::TimeNs)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equality_score(ty: &DataType) -> Option<u8> {
    use DataType::*;
    Some(match ty {
        Null => 0,
        Boolean => 10,
        UTinyInt => 11,
        TinyInt => 12,
        USmallInt => 13,
        SmallInt => 14,
        UInteger => 15,
        Integer => 16,
        UBigInt => 17,
        BigInt => 18,
        UHugeInt => 19,
        HugeInt => 20,
        Decimal { .. } => 21,
        Float => 22,
        Double => 23,
        Time | TimeTz => 50,
        TimeNs => 51,
        Date => 52,
        TimestampS => 53,
        TimestampMs => 54,
        Timestamp | TimestampTz => 55,
        TimestampNs | TimestampTzNs => 56,
        Interval => 58,
        Varchar => 77,
        Enum(_) => 78,
        Bit => 100,
        Blob => 101,
        Uuid => 102,
        Bignum => 103,
        Nested(_) | Extension(_) => return None,
    })
}
