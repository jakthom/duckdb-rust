use std::sync::Arc;

use super::{
    AggregateFunction, AggregateState, FunctionRegistry, ScalarBindArguments, ScalarFunction,
    operator::{self, Operator},
};
use crate::{
    common::{BitString, DataType, Error, Result, Value, type_registry::TypeRegistry},
    parallel::QueryContext,
};

#[derive(Debug)]
struct BitFunction {
    name: &'static str,
    input: Option<DataType>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for BitFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let types = (0..arguments.len())
            .map(|i| arguments.data_type(i))
            .collect::<Result<Vec<_>>>()?;
        let mut contextual = None;
        if self.name == "xor" && types.len() == 2 {
            for (literal, other) in [(0, 1), (1, 0)] {
                let target = &types[other];
                let fits = arguments.integer_literal(literal)?.is_some_and(|value| {
                    if target.is_unsigned_integer() {
                        value >= 0 && Value::Unsigned(value as u128).fits_type(target)
                    } else {
                        target.is_signed_integer() && Value::Integer(value).fits_type(target)
                    }
                });
                if fits || (*target == DataType::Bit && arguments.is_string_literal(literal)?) {
                    contextual = Some(vec![target.clone(); 2]);
                    break;
                }
            }
        }
        let targets = match contextual {
            Some(targets) => targets,
            None => self.argument_types(&types, query.types())?,
        };
        self.return_type(&targets, query.types())?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            input: targets.first().cloned(),
        })))
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Vec<DataType>> {
        use DataType::*;
        if self.name == "xor"
            && arguments.len() == 2
            && let Some(input) = &self.input
        {
            return Ok(vec![input.clone(); 2]);
        }
        Ok(match (self.name, arguments) {
            ("get_bit", [_, _]) => vec![Bit, Integer],
            ("set_bit", [_, _, _]) => vec![Bit, Integer, Integer],
            ("bit_position", [_, _]) => vec![Bit, Bit],
            ("bitstring_byte_comparable", [_]) => vec![Bit],
            ("bitstring", [Bit, _]) => vec![Bit, Integer],
            ("bitstring", [Varchar | Null | Enum(_), _]) => vec![Varchar, Integer],
            ("bit_length", [Null | Enum(_)]) => vec![Varchar],
            ("bit_count", [Null]) => vec![TinyInt],
            ("bit_count", [input]) if input.is_unsigned_integer() => vec![match input {
                UTinyInt => SmallInt,
                USmallInt => Integer,
                UInteger => BigInt,
                UBigInt => HugeInt,
                _ => return Err(Error::Bind("no bit_count overload for UHUGEINT".into())),
            }],
            ("xor", [left, right]) => {
                let ty = if *left == Null && *right == Null {
                    BigInt
                } else {
                    types.common_type(left, right)?
                };
                vec![ty; 2]
            }
            _ => arguments.to_vec(),
        })
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        use DataType::*;
        match (self.name, arguments) {
            ("bitstring", [Bit | Varchar, Integer]) => Ok(Bit),
            ("get_bit", [Bit, Integer]) | ("bit_position", [Bit, Bit]) => Ok(Integer),
            ("set_bit", [Bit, Integer, Integer]) => Ok(Bit),
            ("bitstring_byte_comparable", [Bit]) => Ok(Blob),
            ("bit_length", [Varchar | Bit]) | ("bit_count", [Bit]) => Ok(BigInt),
            ("bit_count", [input]) if input.is_signed_integer() => Ok(TinyInt),
            ("xor", [left, right]) if left == right && (left.is_integer() || *left == Bit) => {
                Ok(left.clone())
            }
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.name
            ))),
        }
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if self.name == "xor" {
            return operator::bitwise::evaluate(
                Operator::BitXor,
                self.input
                    .as_ref()
                    .ok_or_else(|| Error::Internal("unbound xor".into()))?,
                arguments,
                query,
            );
        }
        if self.name == "bitstring" {
            let length = arguments[1].as_i128()?;
            if length < 0 {
                return Err(Error::InvalidInput(
                    "The bitstring length cannot be negative".into(),
                ));
            }
            let length = usize::try_from(length)
                .map_err(|_| Error::Resource("BIT length overflow".into()))?;
            let bits = match &arguments[0] {
                Value::Varchar(text) => {
                    if length < text.len() {
                        return Err(Error::InvalidInput(
                            "Length must be equal or larger than input string".into(),
                        ));
                    }
                    if text.is_empty() || text.bytes().any(|b| !matches!(b, b'0' | b'1')) {
                        return Err(Error::Conversion("invalid input BIT string".into()));
                    }
                    BitString::parse(text, || query.check())?
                }
                Value::Bit(bits) => (**bits).clone(),
                _ => return Err(Error::Internal("bitstring input type".into())),
            };
            return bits.extend(length, || query.check()).map(BitString::value);
        }
        if let Value::Varchar(text) = &arguments[0]
            && self.name == "bit_length"
        {
            return Ok(Value::Integer(text.len() as i128 * 8));
        }
        if let Value::Integer(value) = arguments[0]
            && self.name == "bit_count"
        {
            let width = self
                .input
                .as_ref()
                .and_then(DataType::integer_bits)
                .ok_or_else(|| Error::Internal("bit_count width".into()))?;
            let bits = value as u128
                & if width == 128 {
                    u128::MAX
                } else {
                    (1_u128 << width) - 1
                };
            // The pinned scalar overload returns TINYINT even for HUGEINT;
            // 128 set bits therefore wrap to -128 in its signed byte result.
            return Ok(Value::Integer(bits.count_ones() as i8 as i128));
        }
        let Value::Bit(bits) = &arguments[0] else {
            return Err(Error::Internal("BIT scalar input".into()));
        };
        Ok(match self.name {
            "bit_length" => Value::Integer(bits.length() as i128),
            "bit_count" => Value::Integer(bits.count(|| query.check())? as i128),
            "get_bit" => Value::Integer(i128::from(
                bits.get(
                    usize::try_from(arguments[1].as_i128()?)
                        .map_err(|_| Error::OutOfRange("bit index outside valid range".into()))?,
                )?,
            )),
            "set_bit" => {
                let bit = arguments[2].as_i128()?;
                if bit != 0 && bit != 1 {
                    return Err(Error::InvalidInput("The new bit must be 1 or 0".into()));
                }
                let index = usize::try_from(arguments[1].as_i128()?)
                    .map_err(|_| Error::OutOfRange("bit index outside valid range".into()))?;
                bits.with_bit(index, bit == 1)?.value()
            }
            "bitstring_byte_comparable" => {
                let mut output = Vec::with_capacity(bits.length());
                for index in 0..bits.length() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(if bits.get(index)? { 3 } else { 2 });
                }
                Value::Blob(output)
            }
            "bit_position" => {
                let Value::Bit(haystack) = &arguments[1] else {
                    return Err(Error::Internal("BIT position input".into()));
                };
                if bits.length() == 0 {
                    return Err(Error::InvalidInput("empty BIT position needle".into()));
                }
                let mut matched = 0;
                let mut result = 0;
                // Development resets to zero after a mismatch without retrying
                // an overlapping prefix; preserve that observed behavior.
                for index in 0..haystack.length() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if haystack.get(index)? == bits.get(matched)? {
                        matched += 1;
                        if matched == bits.length() {
                            result = index + 2 - matched;
                            break;
                        }
                    } else {
                        matched = 0;
                    }
                }
                Value::Integer(result as i128)
            }
            _ => return Err(Error::Internal("unknown BIT function".into())),
        })
    }
}

#[derive(Debug)]
struct BitAggregate(&'static str, Operator);
struct BitState {
    operator: Operator,
    data_type: DataType,
    value: Value,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for BitAggregate {
    fn name(&self) -> &str {
        self.0
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        match arguments {
            [DataType::Null] => Ok(DataType::BigInt),
            [ty] if ty.is_integer() || *ty == DataType::Bit => Ok(ty.clone()),
            _ => Err(Error::Bind(
                "bitwise aggregate needs integer or BIT input".into(),
            )),
        }
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        Ok(Box::new(BitState {
            operator: self.1,
            data_type: self.return_type(arguments, types)?,
            value: Value::Null,
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for BitState {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("bitwise aggregate arity".into()));
        };
        if value.is_null() {
            return Ok(());
        }
        self.value = if self.value.is_null() {
            value.clone()
        } else {
            operator::bitwise::evaluate(
                self.operator,
                &self.data_type,
                &[self.value.clone(), value.clone()],
                query,
            )?
        };
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(self.value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "bitstring",
        "bit_length",
        "bit_count",
        "get_bit",
        "set_bit",
        "bit_position",
        "bitstring_byte_comparable",
        "xor",
    ] {
        registry
            .register_scalar(Arc::new(BitFunction { name, input: None }))
            .expect("unique BIT scalar function");
    }
    for (name, operator) in [
        ("bit_and", Operator::BitAnd),
        ("bit_or", Operator::BitOr),
        ("bit_xor", Operator::BitXor),
    ] {
        registry
            .register_aggregate(Arc::new(BitAggregate(name, operator)))
            .expect("unique bitwise aggregate");
    }
}
