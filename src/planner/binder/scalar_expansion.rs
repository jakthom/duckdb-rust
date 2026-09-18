//! Bound-tree budget for selected expansion. Template-only limits cannot bound
//! cloning a previously expanded argument. No value or callback is evaluated.
use super::*;
use crate::{
    function::{ScalarExpansion, ScalarExpansionNode as N},
    parallel::QueryContext,
};

#[derive(Clone, Copy)]
struct Weight {
    nodes: usize,
    depth: usize,
}

const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 128;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn limit() -> Error {
    Error::Resource("scalar expansion expanded bound-tree node/depth limit".into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn argument_weight(argument: &BoundExpr, query: &QueryContext) -> Result<Weight> {
    let mut pending = vec![(argument, 1usize)];
    let mut result = Weight { nodes: 0, depth: 0 };
    while let Some((expression, depth)) = pending.pop() {
        query.check()?;
        result.nodes += 1;
        result.depth = result.depth.max(depth);
        if result.nodes > MAX_NODES || depth > MAX_DEPTH {
            return Err(limit());
        }
        let mut overflow = false;
        expression.visit_children(&mut |child| {
            // Do not first allocate a huge traversal stack for a wide argument.
            if result.nodes + pending.len() >= MAX_NODES {
                overflow = true;
            } else {
                pending.push((child, depth + 1));
            }
        });
        if overflow {
            return Err(limit());
        }
    }
    Ok(result)
}

/// Complete template validation must precede this pass. Measure actual retained
/// argument subtrees before cloning any occurrence, including nested selected
/// expansions. Shared subquery plans are not copied; their scalar needle is
/// conservatively counted by the ordinary scalar child visitor.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn validate_bound_expansion(
    expansion: &ScalarExpansion,
    arguments: &[BoundExpr],
    query: &QueryContext,
) -> Result<()> {
    query.check()?;
    let mut arguments_seen = BTreeMap::new();
    let mut weights: Vec<Weight> = Vec::with_capacity(expansion.nodes.len());
    for node in &expansion.nodes {
        query.check()?;
        let weight = match node {
            N::Argument(index) => {
                if let Some(weight) = arguments_seen.get(index) {
                    *weight
                } else {
                    let weight = argument_weight(&arguments[*index], query)?;
                    arguments_seen.insert(*index, weight);
                    weight
                }
            }
            N::Null => Weight { nodes: 1, depth: 1 },
            _ => {
                let children: &[usize] = match node {
                    N::Equal { left, right } => &[*left, *right],
                    N::Case {
                        condition,
                        then_value,
                        otherwise,
                    } => &[*condition, *then_value, *otherwise],
                    _ => unreachable!("leaf weights handled above"),
                };
                // Ordinary comparison/CASE binding may insert one retained cast
                // per child edge. Reserve that cost before lowering or pruning.
                let mut weight = Weight { nodes: 1, depth: 1 };
                for &child in children {
                    weight.nodes += weights[child].nodes + 1;
                    weight.depth = weight.depth.max(weights[child].depth + 2);
                }
                weight
            }
        };
        if weight.nodes > MAX_NODES || weight.depth > MAX_DEPTH {
            return Err(limit());
        }
        weights.push(weight);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn bound_argument_and_template_depths_are_combined_before_lowering() -> Result<()> {
        let query = QueryContext::background();
        let mut argument = BoundExpr::literal(Value::Boolean(true));
        for _ in 0..80 {
            argument = BoundExpr {
                data_type: DataType::Boolean,
                kind: ExprKind::Unary(UnaryOp::Not, Box::new(argument)),
            };
        }
        let mut nodes = vec![N::Argument(0), N::Null];
        for _ in 0..30 {
            nodes.push(N::Equal {
                left: nodes.len() - 1,
                right: 0,
            });
        }
        let expansion = ScalarExpansion { nodes };
        expansion.validate(1, &query)?;
        assert!(matches!(
            validate_bound_expansion(&expansion, &[argument], &query),
            Err(Error::Resource(_))
        ));
        Ok(())
    }
}
