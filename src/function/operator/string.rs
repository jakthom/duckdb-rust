use super::*;

const MAX_BYTES: usize = 16 * 1024 * 1024;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn inputs(arguments: &[Value]) -> Result<(&str, &str)> {
    match arguments {
        [Value::Varchar(value), Value::Varchar(pattern)] => {
            if value.len() > MAX_BYTES || pattern.len() > MAX_BYTES {
                return Err(Error::Resource(
                    "string operator input exceeds 16 MiB".into(),
                ));
            }
            Ok((value, pattern))
        }
        _ => Err(Error::Internal("invalid string operator arguments".into())),
    }
}

#[derive(Debug)]
pub struct Concatenate;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for Concatenate {
    fn name(&self) -> &'static str {
        "string-concatenate"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        signature.operator == Operator::Concat
            && signature.arguments == [DataType::Varchar, DataType::Varchar]
            && signature.result == DataType::Varchar
            && !signature.nullable
    }
    fn coercion(&self) -> CastMode {
        CastMode::Assignment
    }
    fn evaluate(
        &self,
        _: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        let (a, b) = inputs(arguments)?;
        let length = a
            .len()
            .checked_add(b.len())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| Error::Resource("concatenation exceeds 16 MiB".into()))?;
        let mut result = String::new();
        result
            .try_reserve_exact(length)
            .map_err(|_| Error::Resource("cannot allocate concatenation".into()))?;
        for mut value in [a, b] {
            while !value.is_empty() {
                query.check()?;
                let mut end = value.len().min(4096);
                while !value.is_char_boundary(end) {
                    end -= 1;
                }
                result.push_str(&value[..end]);
                value = &value[end..];
            }
        }
        Ok(Value::Varchar(result))
    }
}

#[derive(Debug)]
pub struct DynamicLike;
#[derive(Debug)]
pub struct GreedyLike;

macro_rules! like_adapter {
    ($adapter:ident, $name:literal, $matcher:ident) => {
        #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
        impl OperatorFunction for $adapter {
            fn name(&self) -> &'static str {
                $name
            }
            fn supports(&self, signature: &OperatorSignature) -> bool {
                matches!(signature.operator, Operator::Like | Operator::NotLike)
                    && signature.arguments == [DataType::Varchar, DataType::Varchar]
                    && signature.result == DataType::Boolean
                    && !signature.nullable
            }
            fn evaluate(
                &self,
                signature: &OperatorSignature,
                arguments: &[Value],
                query: &QueryContext,
            ) -> Result<Value> {
                query.check()?;
                let (value, pattern) = inputs(arguments)?;
                Ok(Value::Boolean(
                    $matcher(value, pattern, query)? ^ (signature.operator == Operator::NotLike),
                ))
            }
        }
    };
}
like_adapter!(DynamicLike, "like-dynamic-programming", dynamic);
like_adapter!(GreedyLike, "like-greedy", greedy);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Dynamic programming over Unicode scalar values; two reusable state rows.
fn dynamic(value: &str, pattern: &str, query: &QueryContext) -> Result<bool> {
    let mut chars = Vec::new();
    chars
        .try_reserve_exact(value.len())
        .map_err(|_| Error::Resource("cannot allocate LIKE characters".into()))?;
    for (i, c) in value.chars().enumerate() {
        if i.is_multiple_of(1024) {
            query.check()?;
        }
        chars.push(c);
    }
    let size = chars.len() + 1;
    let mut previous = Vec::new();
    let mut next = Vec::new();
    previous
        .try_reserve_exact(size)
        .map_err(|_| Error::Resource("cannot allocate LIKE states".into()))?;
    next.try_reserve_exact(size)
        .map_err(|_| Error::Resource("cannot allocate LIKE states".into()))?;
    previous.resize(size, false);
    next.resize(size, false);
    previous[0] = true;
    for token in pattern.chars() {
        query.check()?;
        next[0] = token == '%' && previous[0];
        for i in 1..size {
            if i.is_multiple_of(1024) {
                query.check()?;
            }
            next[i] = if token == '%' {
                previous[i] || next[i - 1]
            } else {
                previous[i - 1] && (token == '_' || token == chars[i - 1])
            };
        }
        std::mem::swap(&mut previous, &mut next);
    }
    Ok(previous[size - 1])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Constant scratch space. A percent remembers the next pattern position and
/// retries its suffix at successive character boundaries. Worst-case time is
/// still O(value * pattern), so retries cooperate with query cancellation.
fn greedy(value: &str, pattern: &str, query: &QueryContext) -> Result<bool> {
    let (mut v, mut p) = (0, 0);
    let mut wildcard: Option<(usize, usize)> = None;
    let mut steps = 0_usize;
    while v < value.len() {
        if steps.is_multiple_of(1024) {
            query.check()?;
        }
        steps += 1;
        // Literal UTF-8 bytes can be compared directly. Reaching a wildcard
        // implies a complete literal prefix matched, hence a character boundary.
        // Only '_' and wildcard retries need to decode a Unicode scalar.
        match pattern.as_bytes().get(p) {
            Some(b'%') => {
                p += 1;
                wildcard = Some((p, v));
            }
            Some(b'_') => {
                p += 1;
                v += value[v..]
                    .chars()
                    .next()
                    .expect("wildcard character boundary")
                    .len_utf8();
            }
            Some(&c) if c == value.as_bytes()[v] => {
                p += 1;
                v += 1;
            }
            _ => {
                let Some((resume, retry)) = &mut wildcard else {
                    return Ok(false);
                };
                let Some(c) = value[*retry..].chars().next() else {
                    return Ok(false);
                };
                *retry += c.len_utf8();
                v = *retry;
                p = *resume;
            }
        }
    }
    while pattern.as_bytes().get(p) == Some(&b'%') {
        if steps.is_multiple_of(1024) {
            query.check()?;
        }
        steps += 1;
        p += 1;
    }
    Ok(p == pattern.len())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut OperatorRegistry) {
    for (operator, result, function) in [
        (
            Operator::Concat,
            DataType::Varchar,
            Arc::new(Concatenate) as Arc<dyn OperatorFunction>,
        ),
        (Operator::Like, DataType::Boolean, Arc::new(GreedyLike)),
        (Operator::NotLike, DataType::Boolean, Arc::new(GreedyLike)),
    ] {
        registry
            .register(
                OperatorSignature {
                    operator,
                    arguments: vec![DataType::Varchar, DataType::Varchar],
                    result,
                    nullable: false,
                },
                function,
            )
            .expect("unique string operator");
    }
}
