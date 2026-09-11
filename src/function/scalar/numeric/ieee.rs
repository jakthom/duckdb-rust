//! IEEE-dependent math retains statement-local mode and selected DOUBLE casts.
use super::*;
use crate::function::{ArgumentEvaluation, ScalarBindArguments, ScalarSignature};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Sqrt,
    Ln,
    Log10,
    Log,
    Log2,
    Power,
    Gamma,
    LogGamma,
}

#[derive(Debug)]
struct Math {
    name: &'static str,
    operation: Operation,
    binding: Option<(bool, ScalarSignature)>,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, operation) in [
        ("sqrt", Operation::Sqrt),
        ("ln", Operation::Ln),
        ("log10", Operation::Log10),
        ("log", Operation::Log),
        ("log2", Operation::Log2),
        ("pow", Operation::Power),
        ("power", Operation::Power),
        ("**", Operation::Power),
        ("^", Operation::Power),
        ("gamma", Operation::Gamma),
        ("lgamma", Operation::LogGamma),
    ] {
        registry
            .register_scalar(Arc::new(Math {
                name,
                operation,
                binding: None,
                known_null: false,
            }))
            .expect("unique IEEE math function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Math {
    fn candidates(&self) -> Vec<ScalarSignature> {
        let counts: &[usize] = match self.operation {
            Operation::Power => &[2],
            Operation::Log => &[1, 2],
            _ => &[1],
        };
        counts
            .iter()
            .map(|&count| ScalarSignature {
                arguments: vec![DataType::Double; count],
                return_type: DataType::Double,
                argument_names: None,
            })
            .collect()
    }
    fn bound(&self) -> Result<&(bool, ScalarSignature)> {
        self.binding.as_ref().ok_or_else(|| {
            Error::Unsupported(
                "IEEE math requires selected statement-local function binding".into(),
            )
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Math {
    fn name(&self) -> &str {
        self.name
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.known_null {
            ArgumentEvaluation::TypeOnly
        } else {
            ArgumentEvaluation::NullOnConstant
        }
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let candidates = self.candidates();
        ScalarSignature::validate_candidates(self.name, &candidates, query)?;
        let selected = arguments.select_overload(self.name, &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if signature.arguments.len() != arguments.len() {
            return Err(Error::Internal(
                "IEEE overload changed argument count".into(),
            ));
        }
        // Native default-NULL binding probes inputs after overload selection,
        // before the IEEE bind callback. A failed recoverable probe can precede
        // a later proven NULL. The selected frontend owns those exact limits.
        let mut known_null = false;
        for index in 0..arguments.len() {
            if arguments.is_provably_null(index)? {
                known_null = true;
                break;
            }
        }
        let ieee = if known_null {
            // Unused: native binding never reads the setting for this path.
            true
        } else {
            match query.settings().get("ieee_floating_point_ops", query)? {
                Value::Boolean(value) => *value,
                // Native Settings::Get<Boolean> uses the declared default for NULL
                // without rewriting the stored/current_setting value.
                Value::Null => true,
                _ => return Err(Error::Internal("IEEE setting is not BOOLEAN".into())),
            }
        };
        query.check()?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            operation: self.operation,
            binding: Some((ieee, signature)),
            known_null,
        })))
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        let signature = &self.bound()?.1;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Bind(
                "IEEE function argument count differs from binding".into(),
            ));
        }
        Ok(signature.arguments.clone())
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        let signature = &self.bound()?.1;
        if arguments != signature.arguments {
            return Err(Error::Bind(
                "IEEE function arguments were not coerced to DOUBLE".into(),
            ));
        }
        Ok(signature.return_type.clone())
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let (ieee, signature) = self.bound()?;
        if self.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(
                    "constant NULL IEEE function received arguments".into(),
                ));
            }
            return Ok(Value::Null);
        }
        if arguments.len() != signature.arguments.len()
            || arguments
                .iter()
                .any(|value| !matches!(value, Value::Double(_) | Value::Null))
        {
            return Err(Error::Internal(
                "IEEE function received invalid arguments".into(),
            ));
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let Value::Double(a) = arguments[0] else {
            unreachable!("checked DOUBLE input")
        };
        let result = match self.operation {
            Operation::Sqrt => {
                if !ieee && a < 0.0 {
                    return Err(Error::OutOfRange(
                        "cannot take square root of a negative number".into(),
                    ));
                }
                a.sqrt()
            }
            Operation::Ln => {
                check_log(a, *ieee)?;
                a.ln()
            }
            Operation::Log10 => {
                check_log(a, *ieee)?;
                a.log10()
            }
            Operation::Log2 => {
                check_log(a, *ieee)?;
                a.log2()
            }
            Operation::Log if arguments.len() == 1 => {
                check_log(a, *ieee)?;
                a.log10()
            }
            Operation::Log => {
                let Value::Double(b) = arguments[1] else {
                    unreachable!("checked DOUBLE input")
                };
                // The strict reference validates the base and its zero log
                // before the value. Do not replace this with a reordered ratio.
                check_log(a, *ieee)?;
                let divisor = a.log10();
                if !ieee && divisor == 0.0 {
                    return Err(Error::OutOfRange(
                        "division by zero in based logarithm".into(),
                    ));
                }
                check_log(b, *ieee)?;
                b.log10() / divisor
            }
            Operation::Power => {
                let Value::Double(b) = arguments[1] else {
                    unreachable!("checked DOUBLE input")
                };
                // Strict POW only rejects zero to a negative power; negative
                // nonintegral powers and overflow still produce NaN/infinity.
                if !ieee && a == 0.0 && b < 0.0 {
                    return Err(Error::OutOfRange(
                        "zero raised to a negative power is undefined".into(),
                    ));
                }
                a.powf(b)
            }
            Operation::Gamma => {
                if !ieee && a == 0.0 {
                    return Err(Error::OutOfRange("cannot take gamma of zero".into()));
                }
                libm::tgamma(a)
            }
            Operation::LogGamma => {
                if !ieee && a == 0.0 {
                    return Err(Error::OutOfRange("cannot take log gamma of zero".into()));
                }
                libm::lgamma(a)
            }
        };
        query.check()?;
        Ok(Value::Double(result))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_log(value: f64, ieee: bool) -> Result<()> {
    if !ieee {
        if value < 0.0 {
            return Err(Error::OutOfRange(
                "cannot take logarithm of a negative number".into(),
            ));
        }
        if value == 0.0 {
            return Err(Error::OutOfRange("cannot take logarithm of zero".into()));
        }
    }
    Ok(())
}
