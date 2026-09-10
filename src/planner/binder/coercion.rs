//! Comparison combination is a binding context, not a global implicit cast.
//! Prefer selected type proposals and retained cast capabilities; built-in SQL
//! equality has a documented fallback that ordering comparisons do not share.
use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn string_literal(value: &BoundExpr) -> bool {
    matches!(&value.kind, ExprKind::Literal(Value::Varchar(_)))
        && value.data_type == DataType::Varchar
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn comparison_cast_mode(
        &self,
        source: &DataType,
        target: &DataType,
    ) -> Result<CastMode> {
        Ok(
            if self
                .context
                .casts
                .coercion_cost_with_types(
                    source,
                    target,
                    CastMode::Implicit,
                    self.context.query.types(),
                )?
                .is_some()
            {
                CastMode::Implicit
            } else {
                CastMode::Explicit
            },
        )
    }
    pub(super) fn comparison_cast(&self, value: BoundExpr, target: &DataType) -> Result<BoundExpr> {
        let mode = self.comparison_cast_mode(&value.data_type, target)?;
        value.cast(
            target.clone(),
            mode,
            self.context.casts,
            self.context.query.types(),
        )
    }
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
        Nested(_) | Extension(_) => return None,
    })
}
