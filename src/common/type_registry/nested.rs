use std::{cmp::Ordering, collections::BTreeSet, ops::Range, sync::Arc};

use super::{
    BoundType, BoundValidationIdentity, KeyContext, KeyWriter, OrderingRepresentation, TypeAdapter,
    TypeAdapterAccess, TypeRegistry, ValueValidation,
};
use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, Result, Value,
        vector::{
            MetadataAdmission, NestedRowRef, ValidatedNestedBaseRef, ValidatedNestedEncodingRef,
            ValidatedVarcharBaseRef, ValidatedVarcharEncodingRef, Vector,
        },
    },
    parallel::{QueryContext, Reservation},
};

const MAX_COMPARISON_PLAN_NODES: usize = 256;
const MAX_COMPARISON_BASE_PAIRS: usize = 64;

#[derive(Default)]
struct PlanCounts {
    locators: usize,
    chunk_roots: usize,
    varchar_bases: usize,
    nested_bases: usize,
    plans: usize,
    pairs: usize,
    children: usize,
    base_pairs: usize,
}

impl PlanCounts {
    fn add(&mut self, slot: &str, count: usize) -> Result<bool> {
        let target = match slot {
            "locators" => &mut self.locators,
            "chunk_roots" => &mut self.chunk_roots,
            "varchar_bases" => &mut self.varchar_bases,
            "nested_bases" => &mut self.nested_bases,
            "plans" => &mut self.plans,
            "pairs" => &mut self.pairs,
            "children" => &mut self.children,
            _ => return Err(Error::Internal("comparison plan count".into())),
        };
        *target = target
            .checked_add(count)
            .ok_or_else(|| Error::Resource("comparison plan size overflow".into()))?;
        let total = self
            .locators
            .checked_add(self.chunk_roots)
            .and_then(|value| value.checked_add(self.varchar_bases))
            .and_then(|value| value.checked_add(self.nested_bases))
            .and_then(|value| value.checked_add(self.plans))
            .and_then(|value| value.checked_add(self.pairs))
            .and_then(|value| value.checked_add(self.children))
            .ok_or_else(|| Error::Resource("comparison plan size overflow".into()))?;
        Ok(total <= MAX_COMPARISON_PLAN_NODES)
    }

    fn add_base_pairs(&mut self, count: usize) -> Result<bool> {
        self.base_pairs = self
            .base_pairs
            .checked_add(count)
            .ok_or_else(|| Error::Resource("comparison plan pair overflow".into()))?;
        Ok(self.base_pairs <= MAX_COMPARISON_BASE_PAIRS)
    }
}

#[derive(Clone, Copy)]
struct LocatedView {
    root: usize,
    base_start: usize,
    base_count: usize,
}

enum LocatorNode<'a> {
    Direct {
        base: usize,
        offset: usize,
        count: usize,
    },
    Dictionary {
        parent: usize,
        selection: &'a [usize],
        offset: usize,
        count: usize,
    },
    Chunks {
        roots: Range<usize>,
        offsets: &'a [usize],
        offset: usize,
        count: usize,
    },
}

enum ComparisonPlanNode {
    Varchar {
        left: LocatedView,
        right: LocatedView,
    },
    Nested {
        left: LocatedView,
        right: LocatedView,
        pair_indices: Range<usize>,
        right_base_count: usize,
    },
}

enum BasePairPlan {
    Struct { children: Range<usize> },
    List { child: usize },
}

struct ComparisonArena<'a> {
    locators: Vec<LocatorNode<'a>>,
    chunk_roots: Vec<usize>,
    varchar_bases: Vec<ValidatedVarcharBaseRef<'a>>,
    nested_bases: Vec<ValidatedNestedBaseRef<'a>>,
    plans: Vec<ComparisonPlanNode>,
    pairs: Vec<BasePairPlan>,
    children: Vec<usize>,
    _reservation: Option<Reservation>,
}

struct PreparedComparison<'a> {
    arena: ComparisonArena<'a>,
    root: usize,
}

impl<'a> PreparedComparison<'a> {
    fn locate(&self, mut root: usize, mut index: usize) -> Option<(usize, usize)> {
        loop {
            match self.arena.locators.get(root)? {
                LocatorNode::Direct {
                    base,
                    offset,
                    count,
                } => {
                    if index >= *count {
                        return None;
                    }
                    return Some((*base, offset.checked_add(index)?));
                }
                LocatorNode::Dictionary {
                    parent,
                    selection,
                    offset,
                    count,
                } => {
                    if index >= *count {
                        return None;
                    }
                    index = *selection.get(offset.checked_add(index)?)?;
                    root = *parent;
                }
                LocatorNode::Chunks {
                    roots,
                    offsets,
                    offset,
                    count,
                } => {
                    if index >= *count {
                        return None;
                    }
                    let physical = offset.checked_add(index)?;
                    let segment = offsets
                        .partition_point(|&end| end <= physical)
                        .saturating_sub(1);
                    root = *self
                        .arena
                        .chunk_roots
                        .get(roots.start.checked_add(segment)?)?;
                    index = physical.checked_sub(*offsets.get(segment)?)?;
                }
            }
        }
    }

    fn compare_any(
        &self,
        plan: usize,
        left_index: usize,
        right_index: usize,
        query: &QueryContext,
    ) -> Result<Ordering> {
        match self
            .arena
            .plans
            .get(plan)
            .ok_or_else(|| Error::Internal("comparison plan index".into()))?
        {
            ComparisonPlanNode::Varchar { left, right } => {
                let (left_base, left_index) = self
                    .locate(left.root, left_index)
                    .ok_or_else(|| Error::Internal("VARCHAR comparison locator".into()))?;
                let (right_base, right_index) = self
                    .locate(right.root, right_index)
                    .ok_or_else(|| Error::Internal("VARCHAR comparison locator".into()))?;
                let left = self
                    .arena
                    .varchar_bases
                    .get(left_base)
                    .copied()
                    .and_then(|base| base.get(left_index))
                    .ok_or_else(|| Error::Internal("VARCHAR comparison row".into()))?;
                let right = self
                    .arena
                    .varchar_bases
                    .get(right_base)
                    .copied()
                    .and_then(|base| base.get(right_index))
                    .ok_or_else(|| Error::Internal("VARCHAR comparison row".into()))?;
                Ok(match (left, right) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    (Some(left), Some(right)) => left.cmp(right),
                })
            }
            ComparisonPlanNode::Nested {
                left,
                right,
                pair_indices,
                right_base_count,
            } => {
                let (left_base, left_index) = self
                    .locate(left.root, left_index)
                    .ok_or_else(|| Error::Internal("nested comparison locator".into()))?;
                let (right_base, right_index) = self
                    .locate(right.root, right_index)
                    .ok_or_else(|| Error::Internal("nested comparison locator".into()))?;
                self.compare_nested_located(
                    left,
                    right,
                    pair_indices,
                    *right_base_count,
                    left_base,
                    left_index,
                    right_base,
                    right_index,
                    query,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_nested_located(
        &self,
        left: &LocatedView,
        right: &LocatedView,
        pair_indices: &Range<usize>,
        right_base_count: usize,
        left_base: usize,
        left_index: usize,
        right_base: usize,
        right_index: usize,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let left_value = self
            .arena
            .nested_bases
            .get(left_base)
            .copied()
            .ok_or_else(|| Error::Internal("nested comparison base".into()))?;
        let right_value = self
            .arena
            .nested_bases
            .get(right_base)
            .copied()
            .ok_or_else(|| Error::Internal("nested comparison base".into()))?;
        match (
            left_value.is_null(left_index),
            right_value.is_null(right_index),
        ) {
            (Some(true), Some(true)) => return Ok(Ordering::Equal),
            (Some(true), Some(false)) => return Ok(Ordering::Greater),
            (Some(false), Some(true)) => return Ok(Ordering::Less),
            (Some(false), Some(false)) => {}
            _ => return Err(Error::Internal("nested comparison validity".into())),
        }
        let left_local = left_base
            .checked_sub(left.base_start)
            .ok_or_else(|| Error::Internal("nested left base range".into()))?;
        let right_local = right_base
            .checked_sub(right.base_start)
            .ok_or_else(|| Error::Internal("nested right base range".into()))?;
        let pair_offset = left_local
            .checked_mul(right_base_count)
            .and_then(|offset| offset.checked_add(right_local))
            .and_then(|offset| pair_indices.start.checked_add(offset))
            .ok_or_else(|| Error::Internal("nested pair index".into()))?;
        let pair_index = *self
            .arena
            .children
            .get(pair_offset)
            .ok_or_else(|| Error::Internal("nested pair map".into()))?;
        match self
            .arena
            .pairs
            .get(pair_index)
            .ok_or_else(|| Error::Internal("nested pair plan".into()))?
        {
            BasePairPlan::Struct { children } => {
                for (field, child) in self.arena.children[children.clone()]
                    .iter()
                    .copied()
                    .enumerate()
                {
                    if field % 1024 == 0 {
                        query.check()?;
                    }
                    let order = self.compare_any(child, left_index, right_index, query)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                Ok(Ordering::Equal)
            }
            BasePairPlan::List { child } => {
                let (
                    ValidatedNestedBaseRef::List {
                        offsets: left_offsets,
                        ..
                    },
                    ValidatedNestedBaseRef::List {
                        offsets: right_offsets,
                        ..
                    },
                ) = (left_value, right_value)
                else {
                    return Err(Error::Internal("nested LIST pair".into()));
                };
                let left_range = *left_offsets
                    .get(left_index)
                    .ok_or_else(|| Error::Internal("nested LIST offset".into()))?
                    ..*left_offsets
                        .get(left_index + 1)
                        .ok_or_else(|| Error::Internal("nested LIST offset".into()))?;
                let right_range = *right_offsets
                    .get(right_index)
                    .ok_or_else(|| Error::Internal("nested LIST offset".into()))?
                    ..*right_offsets
                        .get(right_index + 1)
                        .ok_or_else(|| Error::Internal("nested LIST offset".into()))?;
                for (position, (left_row, right_row)) in
                    left_range.clone().zip(right_range.clone()).enumerate()
                {
                    if position % 1024 == 0 {
                        query.check()?;
                    }
                    let order = self.compare_any(*child, left_row, right_row, query)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                Ok(left_range.len().cmp(&right_range.len()))
            }
        }
    }

    fn compare_top(&self, index: usize, query: &QueryContext) -> Result<Option<Ordering>> {
        let ComparisonPlanNode::Nested { left, right, .. } = self
            .arena
            .plans
            .get(self.root)
            .ok_or_else(|| Error::Internal("top nested comparison plan".into()))?
        else {
            return Err(Error::Internal("top comparison plan is not nested".into()));
        };
        let (left_base, left_index) = self
            .locate(left.root, index)
            .ok_or_else(|| Error::Internal("top left comparison locator".into()))?;
        let (right_base, right_index) = self
            .locate(right.root, index)
            .ok_or_else(|| Error::Internal("top right comparison locator".into()))?;
        let left_null = self
            .arena
            .nested_bases
            .get(left_base)
            .copied()
            .and_then(|base| base.is_null(left_index))
            .ok_or_else(|| Error::Internal("top left comparison validity".into()))?;
        let right_null = self
            .arena
            .nested_bases
            .get(right_base)
            .copied()
            .and_then(|base| base.is_null(right_index))
            .ok_or_else(|| Error::Internal("top right comparison validity".into()))?;
        if left_null || right_null {
            return Ok(None);
        }
        let ComparisonPlanNode::Nested {
            pair_indices,
            right_base_count,
            ..
        } = &self.arena.plans[self.root]
        else {
            unreachable!("top comparison plan checked above")
        };
        self.compare_nested_located(
            left,
            right,
            pair_indices,
            *right_base_count,
            left_base,
            left_index,
            right_base,
            right_index,
            query,
        )
        .map(Some)
    }
}

fn preflight_varchar_view(
    vector: &Vector,
    counts: &mut PlanCounts,
    query: &QueryContext,
) -> Result<Option<usize>> {
    query.check()?;
    let Some(encoding) = vector.validated_varchar_encoding() else {
        return Ok(None);
    };
    if !counts.add("locators", 1)? {
        return Ok(None);
    }
    match encoding {
        ValidatedVarcharEncodingRef::Direct { .. } => {
            Ok(counts.add("varchar_bases", 1)?.then_some(1))
        }
        ValidatedVarcharEncodingRef::Dictionary { parent, .. } => {
            preflight_varchar_view(parent, counts, query)
        }
        ValidatedVarcharEncodingRef::Chunks { chunks, .. } => {
            if !counts.add("chunk_roots", chunks.len())? {
                return Ok(None);
            }
            let mut bases = 0usize;
            for chunk in chunks {
                let Some(count) = preflight_varchar_view(chunk, counts, query)? else {
                    return Ok(None);
                };
                bases = bases
                    .checked_add(count)
                    .ok_or_else(|| Error::Resource("comparison plan size overflow".into()))?;
            }
            Ok(Some(bases))
        }
    }
}

fn preflight_nested_view(
    vector: &Vector,
    counts: &mut PlanCounts,
    query: &QueryContext,
) -> Result<Option<usize>> {
    query.check()?;
    let Some(encoding) = vector.validated_nested_encoding() else {
        return Ok(None);
    };
    if !counts.add("locators", 1)? {
        return Ok(None);
    }
    match encoding {
        ValidatedNestedEncodingRef::Direct { .. } => {
            Ok(counts.add("nested_bases", 1)?.then_some(1))
        }
        ValidatedNestedEncodingRef::Dictionary { parent, .. } => {
            preflight_nested_view(parent, counts, query)
        }
        ValidatedNestedEncodingRef::Chunks { chunks, .. } => {
            if !counts.add("chunk_roots", chunks.len())? {
                return Ok(None);
            }
            let mut bases = 0usize;
            for chunk in chunks {
                let Some(count) = preflight_nested_view(chunk, counts, query)? else {
                    return Ok(None);
                };
                bases = bases
                    .checked_add(count)
                    .ok_or_else(|| Error::Resource("comparison plan size overflow".into()))?;
            }
            Ok(Some(bases))
        }
    }
}

fn visit_nested_bases<'a>(
    vector: &'a Vector,
    query: &QueryContext,
    visitor: &mut impl FnMut(ValidatedNestedBaseRef<'a>) -> Result<bool>,
) -> Result<bool> {
    query.check()?;
    let Some(encoding) = vector.validated_nested_encoding() else {
        return Ok(false);
    };
    match encoding {
        ValidatedNestedEncodingRef::Direct { base, .. } => visitor(base),
        ValidatedNestedEncodingRef::Dictionary { parent, .. } => {
            visit_nested_bases(parent, query, visitor)
        }
        ValidatedNestedEncodingRef::Chunks { chunks, .. } => {
            for chunk in chunks {
                if !visit_nested_bases(chunk, query, visitor)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

fn retained_nested_children(bound: &BoundType) -> Option<&[BoundType]> {
    bound
        .adapter
        .retained_nested_comparison_children(TypeAdapterAccess(()))
}

fn preflight_bound_plan(
    bound: &BoundType,
    left: &Vector,
    right: &Vector,
    counts: &mut PlanCounts,
    query: &QueryContext,
) -> Result<bool> {
    if bound.requires_logical_validation() {
        return Ok(false);
    }
    if bound.ordering_representation() == OrderingRepresentation::VarcharBytes {
        if preflight_varchar_view(left, counts, query)?.is_none()
            || preflight_varchar_view(right, counts, query)?.is_none()
        {
            return Ok(false);
        }
        return counts.add("plans", 1);
    }
    let DataType::Nested(metadata) = bound.data_type() else {
        return Ok(false);
    };
    let Some(children) = retained_nested_children(bound) else {
        return Ok(false);
    };
    preflight_nested_plan(metadata, children, left, right, counts, query)
}

fn preflight_nested_plan(
    metadata: &NestedType,
    children: &[BoundType],
    left: &Vector,
    right: &Vector,
    counts: &mut PlanCounts,
    query: &QueryContext,
) -> Result<bool> {
    if !matches!(metadata, NestedType::Struct(_) | NestedType::List(_)) {
        return Ok(false);
    }
    let Some(left_bases) = preflight_nested_view(left, counts, query)? else {
        return Ok(false);
    };
    let Some(right_bases) = preflight_nested_view(right, counts, query)? else {
        return Ok(false);
    };
    let pair_count = left_bases
        .checked_mul(right_bases)
        .ok_or_else(|| Error::Resource("comparison plan pair overflow".into()))?;
    if !counts.add_base_pairs(pair_count)?
        || !counts.add("pairs", pair_count)?
        || !counts.add("children", pair_count)?
        || !counts.add("plans", 1)?
    {
        return Ok(false);
    }
    let mut supported = true;
    let mut left_visitor = |left_base| {
        let mut right_visitor = |right_base| {
            supported = match (metadata, left_base, right_base) {
                (
                    NestedType::Struct(fields),
                    ValidatedNestedBaseRef::Struct { children: left, .. },
                    ValidatedNestedBaseRef::Struct {
                        children: right, ..
                    },
                ) => {
                    if fields.len() != children.len()
                        || left.len() != children.len()
                        || right.len() != children.len()
                        || !counts.add("children", children.len())?
                    {
                        false
                    } else {
                        let mut result = true;
                        for ((bound, left), right) in children.iter().zip(left).zip(right) {
                            if !preflight_bound_plan(bound, left, right, counts, query)? {
                                result = false;
                                break;
                            }
                        }
                        result
                    }
                }
                (
                    NestedType::List(_),
                    ValidatedNestedBaseRef::List {
                        child: left_child, ..
                    },
                    ValidatedNestedBaseRef::List {
                        child: right_child, ..
                    },
                ) if children.len() == 1 => {
                    preflight_bound_plan(&children[0], left_child, right_child, counts, query)?
                }
                _ => false,
            };
            Ok(supported)
        };
        if !visit_nested_bases(right, query, &mut right_visitor)? {
            supported = false;
        }
        Ok(supported)
    };
    if !visit_nested_bases(left, query, &mut left_visitor)? {
        supported = false;
    }
    Ok(supported)
}

struct PlanBuilder<'a> {
    arena: ComparisonArena<'a>,
}

impl<'a> PlanBuilder<'a> {
    fn new(counts: &PlanCounts, query: &QueryContext) -> Result<Self> {
        let mut admission = MetadataAdmission::new();
        let mut arena = ComparisonArena {
            locators: Vec::new(),
            chunk_roots: Vec::new(),
            varchar_bases: Vec::new(),
            nested_bases: Vec::new(),
            plans: Vec::new(),
            pairs: Vec::new(),
            children: Vec::new(),
            _reservation: None,
        };
        admission.try_reserve_vec(
            &mut arena.locators,
            counts.locators,
            query,
            "comparison plan allocation failed",
        )?;
        admission.try_reserve_vec(
            &mut arena.chunk_roots,
            counts.chunk_roots,
            query,
            "comparison plan allocation failed",
        )?;
        admission.try_reserve_vec(
            &mut arena.varchar_bases,
            counts.varchar_bases,
            query,
            "comparison plan allocation failed",
        )?;
        admission.try_reserve_vec(
            &mut arena.nested_bases,
            counts.nested_bases,
            query,
            "comparison plan allocation failed",
        )?;
        admission.try_reserve_vec(
            &mut arena.plans,
            counts.plans,
            query,
            "comparison plan allocation failed",
        )?;
        admission.try_reserve_vec(
            &mut arena.pairs,
            counts.pairs,
            query,
            "comparison plan allocation failed",
        )?;
        admission.try_reserve_vec(
            &mut arena.children,
            counts.children,
            query,
            "comparison plan allocation failed",
        )?;
        let actual = arena
            .locators
            .capacity()
            .checked_mul(std::mem::size_of::<LocatorNode<'_>>())
            .and_then(|bytes| {
                arena
                    .chunk_roots
                    .capacity()
                    .checked_mul(std::mem::size_of::<usize>())
                    .and_then(|value| bytes.checked_add(value))
            })
            .and_then(|bytes| {
                arena
                    .varchar_bases
                    .capacity()
                    .checked_mul(std::mem::size_of::<ValidatedVarcharBaseRef<'_>>())
                    .and_then(|value| bytes.checked_add(value))
            })
            .and_then(|bytes| {
                arena
                    .nested_bases
                    .capacity()
                    .checked_mul(std::mem::size_of::<ValidatedNestedBaseRef<'_>>())
                    .and_then(|value| bytes.checked_add(value))
            })
            .and_then(|bytes| {
                arena
                    .plans
                    .capacity()
                    .checked_mul(std::mem::size_of::<ComparisonPlanNode>())
                    .and_then(|value| bytes.checked_add(value))
            })
            .and_then(|bytes| {
                arena
                    .pairs
                    .capacity()
                    .checked_mul(std::mem::size_of::<BasePairPlan>())
                    .and_then(|value| bytes.checked_add(value))
            })
            .and_then(|bytes| {
                arena
                    .children
                    .capacity()
                    .checked_mul(std::mem::size_of::<usize>())
                    .and_then(|value| bytes.checked_add(value))
            })
            .ok_or_else(|| Error::Resource("comparison plan size overflow".into()))?;
        arena._reservation = admission.finish(actual, query)?;
        Ok(Self { arena })
    }

    fn push_locator(&mut self, node: LocatorNode<'a>) -> usize {
        let index = self.arena.locators.len();
        self.arena.locators.push(node);
        index
    }

    fn build_varchar_view(&mut self, vector: &'a Vector) -> Result<Option<LocatedView>> {
        let Some(encoding) = vector.validated_varchar_encoding() else {
            return Ok(None);
        };
        Ok(Some(match encoding {
            ValidatedVarcharEncodingRef::Direct {
                base,
                offset,
                count,
            } => {
                let base_index = self.arena.varchar_bases.len();
                self.arena.varchar_bases.push(base);
                let root = self.push_locator(LocatorNode::Direct {
                    base: base_index,
                    offset,
                    count,
                });
                LocatedView {
                    root,
                    base_start: base_index,
                    base_count: 1,
                }
            }
            ValidatedVarcharEncodingRef::Dictionary {
                parent,
                selection,
                offset,
                count,
            } => {
                let Some(parent) = self.build_varchar_view(parent)? else {
                    return Ok(None);
                };
                let root = self.push_locator(LocatorNode::Dictionary {
                    parent: parent.root,
                    selection,
                    offset,
                    count,
                });
                LocatedView { root, ..parent }
            }
            ValidatedVarcharEncodingRef::Chunks {
                chunks,
                offsets,
                offset,
                count,
            } => {
                let base_start = self.arena.varchar_bases.len();
                let roots_start = self.arena.chunk_roots.len();
                self.arena
                    .chunk_roots
                    .resize(roots_start + chunks.len(), usize::MAX);
                for (segment, chunk) in chunks.iter().enumerate() {
                    let Some(view) = self.build_varchar_view(chunk)? else {
                        return Ok(None);
                    };
                    self.arena.chunk_roots[roots_start + segment] = view.root;
                }
                let roots_end = roots_start + chunks.len();
                let root = self.push_locator(LocatorNode::Chunks {
                    roots: roots_start..roots_end,
                    offsets,
                    offset,
                    count,
                });
                LocatedView {
                    root,
                    base_start,
                    base_count: self.arena.varchar_bases.len() - base_start,
                }
            }
        }))
    }

    fn build_nested_view(&mut self, vector: &'a Vector) -> Result<Option<LocatedView>> {
        let Some(encoding) = vector.validated_nested_encoding() else {
            return Ok(None);
        };
        Ok(Some(match encoding {
            ValidatedNestedEncodingRef::Direct {
                base,
                offset,
                count,
            } => {
                let base_index = self.arena.nested_bases.len();
                self.arena.nested_bases.push(base);
                let root = self.push_locator(LocatorNode::Direct {
                    base: base_index,
                    offset,
                    count,
                });
                LocatedView {
                    root,
                    base_start: base_index,
                    base_count: 1,
                }
            }
            ValidatedNestedEncodingRef::Dictionary {
                parent,
                selection,
                offset,
                count,
            } => {
                let Some(parent) = self.build_nested_view(parent)? else {
                    return Ok(None);
                };
                let root = self.push_locator(LocatorNode::Dictionary {
                    parent: parent.root,
                    selection,
                    offset,
                    count,
                });
                LocatedView { root, ..parent }
            }
            ValidatedNestedEncodingRef::Chunks {
                chunks,
                offsets,
                offset,
                count,
            } => {
                let base_start = self.arena.nested_bases.len();
                let roots_start = self.arena.chunk_roots.len();
                self.arena
                    .chunk_roots
                    .resize(roots_start + chunks.len(), usize::MAX);
                for (segment, chunk) in chunks.iter().enumerate() {
                    let Some(view) = self.build_nested_view(chunk)? else {
                        return Ok(None);
                    };
                    self.arena.chunk_roots[roots_start + segment] = view.root;
                }
                let roots_end = roots_start + chunks.len();
                let root = self.push_locator(LocatorNode::Chunks {
                    roots: roots_start..roots_end,
                    offsets,
                    offset,
                    count,
                });
                LocatedView {
                    root,
                    base_start,
                    base_count: self.arena.nested_bases.len() - base_start,
                }
            }
        }))
    }

    fn build_bound_plan(
        &mut self,
        bound: &BoundType,
        left: &'a Vector,
        right: &'a Vector,
    ) -> Result<Option<usize>> {
        if bound.ordering_representation() == OrderingRepresentation::VarcharBytes {
            let (Some(left), Some(right)) = (
                self.build_varchar_view(left)?,
                self.build_varchar_view(right)?,
            ) else {
                return Ok(None);
            };
            let index = self.arena.plans.len();
            self.arena
                .plans
                .push(ComparisonPlanNode::Varchar { left, right });
            return Ok(Some(index));
        }
        let DataType::Nested(metadata) = bound.data_type() else {
            return Ok(None);
        };
        let Some(children) = retained_nested_children(bound) else {
            return Ok(None);
        };
        self.build_nested_plan(metadata, children, left, right)
    }

    fn build_nested_plan(
        &mut self,
        metadata: &NestedType,
        children: &[BoundType],
        left_vector: &'a Vector,
        right_vector: &'a Vector,
    ) -> Result<Option<usize>> {
        let (Some(left), Some(right)) = (
            self.build_nested_view(left_vector)?,
            self.build_nested_view(right_vector)?,
        ) else {
            return Ok(None);
        };
        let pair_count = left
            .base_count
            .checked_mul(right.base_count)
            .ok_or_else(|| Error::Resource("comparison plan pair overflow".into()))?;
        let pair_map_start = self.arena.children.len();
        self.arena
            .children
            .resize(pair_map_start + pair_count, usize::MAX);
        for left_local in 0..left.base_count {
            for right_local in 0..right.base_count {
                let left_base = self.arena.nested_bases[left.base_start + left_local];
                let right_base = self.arena.nested_bases[right.base_start + right_local];
                let pair = match (metadata, left_base, right_base) {
                    (
                        NestedType::Struct(fields),
                        ValidatedNestedBaseRef::Struct {
                            children: left_children,
                            ..
                        },
                        ValidatedNestedBaseRef::Struct {
                            children: right_children,
                            ..
                        },
                    ) => {
                        if fields.len() != children.len()
                            || left_children.len() != children.len()
                            || right_children.len() != children.len()
                        {
                            return Err(Error::Internal("nested STRUCT plan width".into()));
                        }
                        let start = self.arena.children.len();
                        self.arena
                            .children
                            .resize(start + children.len(), usize::MAX);
                        for (field, ((bound, left), right)) in children
                            .iter()
                            .zip(left_children)
                            .zip(right_children)
                            .enumerate()
                        {
                            let Some(child) = self.build_bound_plan(bound, left, right)? else {
                                return Ok(None);
                            };
                            self.arena.children[start + field] = child;
                        }
                        BasePairPlan::Struct {
                            children: start..start + children.len(),
                        }
                    }
                    (
                        NestedType::List(_),
                        ValidatedNestedBaseRef::List {
                            child: left_child, ..
                        },
                        ValidatedNestedBaseRef::List {
                            child: right_child, ..
                        },
                    ) if children.len() == 1 => {
                        let Some(child) =
                            self.build_bound_plan(&children[0], left_child, right_child)?
                        else {
                            return Ok(None);
                        };
                        BasePairPlan::List { child }
                    }
                    _ => return Ok(None),
                };
                let pair_index = self.arena.pairs.len();
                self.arena.pairs.push(pair);
                let map = left_local * right.base_count + right_local;
                self.arena.children[pair_map_start + map] = pair_index;
            }
        }
        let index = self.arena.plans.len();
        self.arena.plans.push(ComparisonPlanNode::Nested {
            left,
            right,
            pair_indices: pair_map_start..pair_map_start + pair_count,
            right_base_count: right.base_count,
        });
        Ok(Some(index))
    }
}

fn prepare_nested_comparison<'a>(
    metadata: &'a NestedType,
    children: &'a [BoundType],
    left: &'a Vector,
    right: &'a Vector,
    query: &QueryContext,
) -> Result<Option<PreparedComparison<'a>>> {
    let mut counts = PlanCounts::default();
    if !preflight_nested_plan(metadata, children, left, right, &mut counts, query)? {
        return Ok(None);
    }
    let mut builder = PlanBuilder::new(&counts, query)?;
    let Some(root) = builder.build_nested_plan(metadata, children, left, right)? else {
        return Err(Error::Internal(
            "comparison plan changed after successful preflight".into(),
        ));
    };
    Ok(Some(PreparedComparison {
        arena: builder.arena,
        root,
    }))
}

#[derive(Debug)]
pub struct NestedTypes {
    children: Vec<BoundType>,
    validation: ValueValidation,
}

impl Default for NestedTypes {
    fn default() -> Self {
        Self {
            children: Vec::new(),
            validation: ValueValidation::Logical,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NestedTypes {
    fn payload<'a>(&self, value: &'a Value) -> Result<&'a NestedPayload> {
        match value {
            Value::Nested(value) => Ok(&value.payload),
            _ => Err(Error::Conversion("expected nested value".into())),
        }
    }
    fn child(&self, index: usize) -> Result<&BoundType> {
        self.children
            .get(index)
            .ok_or_else(|| Error::Internal("nested adapter was not bound".into()))
    }
    fn compare_child(
        &self,
        index: usize,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        match (left.is_null(), right.is_null()) {
            (true, true) => Ok(Ordering::Equal),
            (true, false) => Ok(Ordering::Greater),
            (false, true) => Ok(Ordering::Less),
            (false, false) => self.child(index)?.compare(left, right, query),
        }
    }
    fn compare_child_validated(
        &self,
        index: usize,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        match (left.is_null(), right.is_null()) {
            (true, true) => Ok(Ordering::Equal),
            (true, false) => Ok(Ordering::Greater),
            (false, true) => Ok(Ordering::Less),
            (false, false) => self.child(index)?.compare_validated(left, right, query),
        }
    }
    fn compare_payload(
        &self,
        left: &Value,
        right: &Value,
        validated_children: bool,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let compare_child = |index, left, right| {
            if validated_children {
                self.compare_child_validated(index, left, right, query)
            } else {
                self.compare_child(index, left, right, query)
            }
        };
        let order = match (self.payload(left)?, self.payload(right)?) {
            (NestedPayload::Sequence(a), NestedPayload::Sequence(b))
            | (NestedPayload::Struct(a), NestedPayload::Struct(b)) => {
                let structure = matches!(self.payload(left)?, NestedPayload::Struct(_));
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    let order = compare_child(if structure { index } else { 0 }, a, b)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                a.len().cmp(&b.len())
            }
            (NestedPayload::Map(a), NestedPayload::Map(b)) => {
                for ((ak, av), (bk, bv)) in a.iter().zip(b) {
                    for (index, a, b) in [(0, ak, bk), (1, av, bv)] {
                        let order = compare_child(index, a, b)?;
                        if order != Ordering::Equal {
                            return Ok(order);
                        }
                    }
                }
                a.len().cmp(&b.len())
            }
            (
                NestedPayload::Union { tag: a, value: av },
                NestedPayload::Union { tag: b, value: bv },
            ) => {
                if a != b {
                    a.cmp(b)
                } else {
                    compare_child(*a, av, bv)?
                }
            }
            _ => return Err(Error::Unsupported("nested comparison payload".into())),
        };
        Ok(order)
    }

    fn compare_columnar_rows(
        &self,
        metadata: &NestedType,
        left: NestedRowRef<'_>,
        right: NestedRowRef<'_>,
        query: &QueryContext,
    ) -> Result<Option<Ordering>> {
        match (left, right) {
            (NestedRowRef::Null, _) | (_, NestedRowRef::Null) => Ok(None),
            (
                NestedRowRef::Struct {
                    children: left,
                    index: left_row,
                },
                NestedRowRef::Struct {
                    children: right,
                    index: right_row,
                },
            ) => {
                let NestedType::Struct(fields) = metadata else {
                    return Err(Error::Internal("columnar STRUCT metadata".into()));
                };
                if left.len() != fields.len() || right.len() != fields.len() {
                    return Err(Error::Internal("columnar STRUCT width differs".into()));
                }
                for (index, (left, right)) in left.iter().zip(right).enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    let order =
                        self.compare_vector_child(index, left, left_row, right, right_row, query)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                Ok(Some(Ordering::Equal))
            }
            (
                NestedRowRef::List {
                    child: left,
                    range: left_range,
                },
                NestedRowRef::List {
                    child: right,
                    range: right_range,
                },
            ) => {
                if !matches!(metadata, NestedType::List(_)) {
                    return Err(Error::Internal("columnar LIST metadata".into()));
                }
                for (position, (left_row, right_row)) in
                    left_range.clone().zip(right_range.clone()).enumerate()
                {
                    if position % 1024 == 0 {
                        query.check()?;
                    }
                    let order =
                        self.compare_vector_child(0, left, left_row, right, right_row, query)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                Ok(Some(left_range.len().cmp(&right_range.len())))
            }
            (left, right) => {
                let (structure, child_count) = match metadata {
                    NestedType::Struct(fields) => (true, fields.len()),
                    NestedType::List(_) => (false, usize::MAX),
                    _ => {
                        return Err(Error::Internal(
                            "columnar nested comparison metadata".into(),
                        ));
                    }
                };
                let left = NestedElements::new(left, structure)?;
                let right = NestedElements::new(right, structure)?;
                if structure && (left.len() != child_count || right.len() != child_count) {
                    return Err(Error::Internal("columnar STRUCT width differs".into()));
                }
                for index in 0..left.len().min(right.len()) {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    let child = if structure { index } else { 0 };
                    let order =
                        self.compare_element(child, left.get(index)?, right.get(index)?, query)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                Ok(Some(left.len().cmp(&right.len())))
            }
        }
    }

    fn compare_vector_child(
        &self,
        child: usize,
        left: &Vector,
        left_index: usize,
        right: &Vector,
        right_index: usize,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let bound = self.child(child)?;
        if bound.ordering_representation() == OrderingRepresentation::VarcharBytes {
            let left = left
                .varchar_at_validated(left_index)
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            let right = right
                .varchar_at_validated(right_index)
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            return Ok(match (left, right) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            });
        }
        bound.compare_vector_at_validated(left, left_index, right, right_index, query)
    }

    fn compare_element(
        &self,
        child: usize,
        left: NestedElement<'_>,
        right: NestedElement<'_>,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let bound = self.child(child)?;
        match (left, right) {
            (NestedElement::Vector(left, li), NestedElement::Vector(right, ri)) => {
                self.compare_vector_child(child, left, li, right, ri, query)
            }
            (NestedElement::Vector(column, index), NestedElement::Value(value)) => {
                bound.compare_vector_value_at_validated(column, index, value, true, query)
            }
            (NestedElement::Value(value), NestedElement::Vector(column, index)) => {
                bound.compare_vector_value_at_validated(column, index, value, false, query)
            }
            (NestedElement::Value(left), NestedElement::Value(right)) => {
                self.compare_child_validated(child, left, right, query)
            }
        }
    }
    fn key_child(
        &self,
        index: usize,
        value: &Value,
        key_context: KeyContext,
        writer: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        let mut bytes = Vec::new();
        self.child(index)?
            .append_key_with_context(value, key_context, &mut bytes, query)?;
        writer.extend_from_slice(&bytes)
    }

    fn write_nested_key(
        &self,
        value: &Value,
        key_context: KeyContext,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        match self.payload(value)? {
            NestedPayload::Sequence(values) | NestedPayload::Struct(values) => {
                output.extend_from_slice(&(values.len() as u64).to_le_bytes())?;
                let structure = matches!(self.payload(value)?, NestedPayload::Struct(_));
                for (index, value) in values.iter().enumerate() {
                    self.key_child(
                        if structure { index } else { 0 },
                        value,
                        key_context,
                        output,
                        query,
                    )?;
                }
            }
            NestedPayload::Map(entries) => {
                output.extend_from_slice(&(entries.len() as u64).to_le_bytes())?;
                for (key, value) in entries {
                    self.key_child(0, key, key_context, output, query)?;
                    self.key_child(1, value, key_context, output, query)?;
                }
            }
            NestedPayload::Union { tag, value } => {
                output.extend_from_slice(&(*tag as u64).to_le_bytes())?;
                self.key_child(*tag, value, key_context, output, query)?;
            }
            NestedPayload::Variant { .. } => {
                return Err(Error::Unsupported(
                    "VARIANT keys are not integrated yet".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for NestedTypes {
    #[allow(private_interfaces)]
    fn bound_validation_identity(&self, _: TypeAdapterAccess) -> Option<BoundValidationIdentity> {
        self.children
            .iter()
            .all(BoundType::has_reusable_builtin_validation)
            .then_some(BoundValidationIdentity::RecursiveNested)
    }
    #[allow(private_interfaces)]
    fn retained_nested_comparison_children(&self, _: TypeAdapterAccess) -> Option<&[BoundType]> {
        Some(&self.children)
    }
    fn supports_index(&self, _: &DataType) -> bool {
        false
    }
    fn value_validation(&self) -> ValueValidation {
        self.validation
    }
    fn name(&self) -> &'static str {
        "recursive-nested-types"
    }
    fn bind_type(
        &self,
        data_type: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<Arc<dyn TypeAdapter>>> {
        let DataType::Nested(metadata) = data_type else {
            return Err(Error::Bind("nested type metadata".into()));
        };
        let children: Vec<BoundType> = metadata
            .children()
            .into_iter()
            .map(|child| types.bind(child))
            .collect::<Result<_>>()?;
        let validation = if matches!(
            metadata.as_ref(),
            NestedType::Map { .. } | NestedType::Variant
        ) || children.iter().any(BoundType::requires_logical_validation)
        {
            ValueValidation::Logical
        } else {
            // `NestedValue::fits_type`, enforced by every Vector constructor,
            // proves list/array cardinality, STRUCT/TUPLE shape, UNION tag and
            // the recursively declared physical child types. Only MAP key
            // uniqueness, unsupported VARIANT payloads and selected logical
            // child adapters need another pass.
            ValueValidation::Physical
        };
        Ok(Some(Arc::new(Self {
            children,
            validation,
        })))
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        let DataType::Nested(metadata) = data_type else {
            return Err(Error::Bind("nested type metadata".into()));
        };
        match metadata.as_ref() {
            NestedType::Array { length, .. } if *length == 0 || *length > 100000 => {
                return Err(Error::Bind(
                    "ARRAY size must be between 1 and 100000".into(),
                ));
            }
            NestedType::Struct(fields) | NestedType::Union(fields) => {
                if matches!(metadata.as_ref(), NestedType::Union(_))
                    && (fields.is_empty() || fields.len() > 256)
                {
                    return Err(Error::Bind("invalid nested field count".into()));
                }
                let mut names = BTreeSet::new();
                for (name, _) in fields {
                    if (name.is_empty() && matches!(metadata.as_ref(), NestedType::Union(_)))
                        || !names.insert(name.to_ascii_lowercase())
                    {
                        return Err(Error::Bind(
                            "nested fields require unique names; UNION names must be nonempty"
                                .into(),
                        ));
                    }
                }
            }
            NestedType::Object(fields) => {
                let mut names = BTreeSet::new();
                for (name, _) in fields {
                    if !names.insert(name) {
                        return Err(Error::Bind(
                            "OBJECT fields require exact unique names".into(),
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn validate_value(&self, _: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        query.check()?;
        match self.payload(value)? {
            NestedPayload::Sequence(values) => {
                for value in values {
                    self.child(0)?.validate(value, query)?;
                }
            }
            NestedPayload::Struct(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.child(index)?.validate(value, query)?;
                }
            }
            NestedPayload::Map(entries) => {
                let mut keys = BTreeSet::new();
                for (key, value) in entries {
                    if key.is_null() {
                        return Err(Error::Conversion("MAP keys cannot be NULL".into()));
                    }
                    let mut bytes = Vec::new();
                    self.child(0)?.append_key(key, &mut bytes, query)?;
                    if !keys.insert(bytes) {
                        return Err(Error::Conversion("MAP keys must be unique".into()));
                    }
                    self.child(1)?.validate(value, query)?;
                }
            }
            NestedPayload::Union { tag, value } => self.child(*tag)?.validate(value, query)?,
            NestedPayload::Variant { .. } => {
                return Err(Error::Unsupported(
                    "VARIANT runtime semantics are not integrated yet".into(),
                ));
            }
        }
        Ok(())
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn common_type_with_registry(
        &self,
        left: &DataType,
        right: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        let (DataType::Nested(a), DataType::Nested(b)) = (left, right) else {
            return Ok(None);
        };
        let result = match (a.as_ref(), b.as_ref()) {
            (NestedType::List(a), NestedType::List(b))
            | (NestedType::List(a), NestedType::Array { element: b, .. })
            | (NestedType::Array { element: a, .. }, NestedType::List(b)) => {
                NestedType::List(types.common_type(a, b)?)
            }
            (
                NestedType::Array {
                    element: a,
                    length: x,
                },
                NestedType::Array {
                    element: b,
                    length: y,
                },
            ) if x == y => NestedType::Array {
                element: types.common_type(a, b)?,
                length: *x,
            },
            (NestedType::Struct(a), NestedType::Struct(b)) => {
                let mut fields = a.clone();
                for (name, ty) in b {
                    if let Some((_, existing)) = fields
                        .iter_mut()
                        .find(|(field, _)| field.eq_ignore_ascii_case(name))
                    {
                        *existing = types.common_type(existing, ty)?;
                    } else {
                        fields.push((name.clone(), ty.clone()));
                    }
                }
                NestedType::Struct(fields)
            }
            (NestedType::Map { key: a, value: x }, NestedType::Map { key: b, value: y }) => {
                NestedType::Map {
                    key: types.common_type(a, b)?,
                    value: types.common_type(x, y)?,
                }
            }
            (NestedType::Tuple(a), NestedType::Tuple(b)) if a.len() == b.len() => {
                NestedType::Tuple(
                    a.iter()
                        .zip(b)
                        .map(|(a, b)| types.common_type(a, b))
                        .collect::<Result<_>>()?,
                )
            }
            (NestedType::Tuple(a), NestedType::Struct(b))
            | (NestedType::Struct(b), NestedType::Tuple(a))
                if a.len() == b.len() =>
            {
                NestedType::Struct(
                    a.iter()
                        .zip(b)
                        .map(|(a, (name, b))| Ok((name.clone(), types.common_type(a, b)?)))
                        .collect::<Result<_>>()?,
                )
            }
            _ => return Ok(None),
        };
        Ok(Some(result.data_type()))
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        self.compare_payload(left, right, false, query)
    }
    #[allow(private_interfaces)]
    fn compare_validated_batch(
        &self,
        _: TypeAdapterAccess,
        data_type: &DataType,
        left: &crate::common::vector::Vector,
        right: &crate::common::vector::Vector,
        query: &QueryContext,
    ) -> Option<Result<Vec<Option<Ordering>>>> {
        let DataType::Nested(metadata) = data_type else {
            return Some(Err(Error::Internal("nested batch comparison type".into())));
        };
        let supported_metadata = matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        );
        if supported_metadata {
            let prepared =
                match prepare_nested_comparison(metadata, &self.children, left, right, query) {
                    Ok(prepared) => prepared,
                    Err(error) => return Some(Err(error)),
                };
            if let Some(prepared) = prepared {
                return Some((|| {
                    let mut output = Vec::new();
                    output.try_reserve_exact(left.len()).map_err(|_| {
                        Error::Resource("nested comparison result allocation failed".into())
                    })?;
                    for index in 0..left.len() {
                        if index % 1024 == 0 {
                            query.check()?;
                        }
                        output.push(prepared.compare_top(index, query)?);
                    }
                    Ok(output)
                })());
            }
        }
        if supported_metadata && left.has_nested_row_access() && right.has_nested_row_access() {
            return Some((|| {
                let mut output = Vec::new();
                output.try_reserve_exact(left.len()).map_err(|_| {
                    Error::Resource("nested comparison result allocation failed".into())
                })?;
                for index in 0..left.len() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(self.compare_columnar_rows(
                        metadata,
                        left.nested_row_at(index).expect("prechecked nested row"),
                        right.nested_row_at(index).expect("prechecked nested row"),
                        query,
                    )?);
                }
                Ok(output)
            })());
        }
        // BoundType::compare_batch validated both complete vectors through this
        // exact adapter and its retained children before dispatching here.
        Some(super::batch::compare_values(left, right, query, |a, b| {
            self.compare_payload(a, b, true, query)
        }))
    }
    #[allow(private_interfaces)]
    #[allow(clippy::too_many_arguments)]
    fn compare_validated_vector_at(
        &self,
        _: TypeAdapterAccess,
        data_type: &DataType,
        left: &Vector,
        left_index: usize,
        right: &Vector,
        right_index: usize,
        query: &QueryContext,
    ) -> Option<Result<Ordering>> {
        let DataType::Nested(metadata) = data_type else {
            return Some(Err(Error::Internal("nested comparison type".into())));
        };
        if !matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        ) {
            return None;
        }
        let (Some(left), Some(right)) = (
            left.nested_row_at(left_index),
            right.nested_row_at(right_index),
        ) else {
            return None;
        };
        Some(
            self.compare_columnar_rows(metadata, left, right, query)
                .and_then(|order| {
                    order.ok_or_else(|| {
                        Error::Internal("validated non-NULL nested row became NULL".into())
                    })
                }),
        )
    }
    #[allow(private_interfaces)]
    #[allow(clippy::too_many_arguments)]
    fn compare_validated_vector_value_at(
        &self,
        _: TypeAdapterAccess,
        data_type: &DataType,
        column: &Vector,
        index: usize,
        value: &Value,
        column_is_left: bool,
        query: &QueryContext,
    ) -> Option<Result<Ordering>> {
        let DataType::Nested(metadata) = data_type else {
            return Some(Err(Error::Internal("nested comparison type".into())));
        };
        if !matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        ) {
            return None;
        }
        let Value::Nested(value) = value else {
            return None;
        };
        let column = column.nested_row_at(index)?;
        let scalar = NestedRowRef::Scalar(&value.payload);
        let (left, right) = if column_is_left {
            (column, scalar)
        } else {
            (scalar, column)
        };
        Some(
            self.compare_columnar_rows(metadata, left, right, query)
                .and_then(|order| {
                    order.ok_or_else(|| {
                        Error::Internal("validated non-NULL nested row became NULL".into())
                    })
                }),
        )
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        self.write_nested_key(value, KeyContext::Equality, output, query)
    }
    fn write_key_with_context(
        &self,
        _: &DataType,
        value: &Value,
        key_context: KeyContext,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        self.write_nested_key(value, key_context, output, query)
    }
}

enum NestedElement<'a> {
    Vector(&'a Vector, usize),
    Value(&'a Value),
}

enum NestedElements<'a> {
    ColumnarStruct {
        children: &'a [Vector],
        index: usize,
    },
    ColumnarList {
        child: &'a Vector,
        range: std::ops::Range<usize>,
    },
    Scalar(&'a [Value]),
}

impl<'a> NestedElements<'a> {
    fn new(row: NestedRowRef<'a>, structure: bool) -> Result<Self> {
        match (structure, row) {
            (true, NestedRowRef::Struct { children, index }) => {
                Ok(Self::ColumnarStruct { children, index })
            }
            (false, NestedRowRef::List { child, range }) => Ok(Self::ColumnarList { child, range }),
            (true, NestedRowRef::Scalar(NestedPayload::Struct(values)))
            | (false, NestedRowRef::Scalar(NestedPayload::Sequence(values))) => {
                Ok(Self::Scalar(values))
            }
            _ => Err(Error::Internal("nested row differs from metadata".into())),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::ColumnarStruct { children, .. } => children.len(),
            Self::ColumnarList { range, .. } => range.len(),
            Self::Scalar(values) => values.len(),
        }
    }

    fn get(&self, index: usize) -> Result<NestedElement<'a>> {
        match self {
            Self::ColumnarStruct {
                children,
                index: row,
            } => children
                .get(index)
                .map(|child| NestedElement::Vector(child, *row))
                .ok_or_else(|| Error::Internal("columnar STRUCT child index".into())),
            Self::ColumnarList { child, range } => range
                .start
                .checked_add(index)
                .filter(|index| *index < range.end)
                .map(|index| NestedElement::Vector(child, index))
                .ok_or_else(|| Error::Internal("columnar LIST child index".into())),
            Self::Scalar(values) => values
                .get(index)
                .map(NestedElement::Value)
                .ok_or_else(|| Error::Internal("scalar nested child index".into())),
        }
    }
}

#[cfg(test)]
mod comparison_plan_tests {
    use super::*;
    use crate::parallel::{InterruptHandle, MemoryPool};

    fn admitted_struct(
        data_type: DataType,
        children: Vec<Vector>,
        query: &QueryContext,
    ) -> Result<Vector> {
        let count = children.first().map_or(0, Vector::len);
        let mut admission = MetadataAdmission::new();
        let mut retained = Vec::new();
        admission.try_reserve_vec(&mut retained, children.len(), query, "test STRUCT metadata")?;
        retained.extend(children);
        Vector::flat_struct_checked(data_type, count, None, retained, admission, query)
    }

    fn admitted_list(
        data_type: DataType,
        offsets: &[usize],
        child: Vector,
        query: &QueryContext,
    ) -> Result<Vector> {
        let mut admission = MetadataAdmission::new();
        let mut retained = Vec::new();
        admission.try_reserve_vec(&mut retained, offsets.len(), query, "test LIST offsets")?;
        retained.extend_from_slice(offsets);
        Vector::flat_list_checked(
            data_type,
            offsets.len() - 1,
            None,
            retained,
            child,
            admission,
            query,
        )
    }

    fn dictionary_over_nested_chunks(
        data_type: DataType,
        vector: Vector,
        selection: Vec<usize>,
    ) -> Result<Vector> {
        let inner = Vector::chunked(data_type.clone(), vec![vector])?;
        let outer = Arc::new(Vector::chunked(data_type, vec![inner])?);
        outer.select(selection)
    }

    fn multi_chunk_dictionary_slice(
        data_type: DataType,
        vector: Vector,
        selection: Vec<usize>,
        offset: usize,
        count: usize,
    ) -> Result<Vector> {
        let midpoint = vector.len() / 2;
        let first = Vector::chunked(
            data_type.clone(),
            vec![
                vector.slice(0, 1)?,
                vector.slice(1, midpoint.saturating_sub(1))?,
            ],
        )?;
        let second = vector.slice(midpoint, vector.len() - midpoint)?;
        let chunks = Vector::chunked(data_type.clone(), vec![first, second])?;
        Arc::new(chunks).select(selection)?.slice(offset, count)
    }

    fn nested_list_vector(values: [&str; 4], query: &QueryContext) -> Result<(DataType, Vector)> {
        let struct_type = NestedType::Struct(vec![("v".into(), DataType::Varchar)]).data_type();
        let list_type = NestedType::List(struct_type.clone()).data_type();
        let text = Vector::flat(
            DataType::Varchar,
            values
                .into_iter()
                .map(|value| Value::Varchar(value.into()))
                .collect(),
        )?;
        let text = dictionary_over_nested_chunks(DataType::Varchar, text, vec![3, 1, 2, 0])?;
        let structs = admitted_struct(struct_type.clone(), vec![text], query)?;
        let structs = dictionary_over_nested_chunks(struct_type, structs, vec![3, 1, 2, 0])?;
        let lists = admitted_list(list_type.clone(), &[0, 2, 4], structs, query)?;
        let lists = dictionary_over_nested_chunks(list_type.clone(), lists, vec![1, 0])?;
        Ok((list_type, lists))
    }

    fn nested_offset_list_vector(query: &QueryContext) -> Result<(DataType, Vector)> {
        let struct_type = NestedType::Struct(vec![("v".into(), DataType::Varchar)]).data_type();
        let list_type = NestedType::List(struct_type.clone()).data_type();
        let text = Vector::flat(
            DataType::Varchar,
            ["a", "b", "c", "d", "e", "f", "g", "h"]
                .into_iter()
                .map(|value| Value::Varchar(value.into()))
                .collect(),
        )?;
        let text = multi_chunk_dictionary_slice(
            DataType::Varchar,
            text,
            vec![7, 0, 6, 1, 5, 2, 4, 3],
            1,
            6,
        )?;
        let structs = admitted_struct(struct_type.clone(), vec![text], query)?;
        let structs =
            multi_chunk_dictionary_slice(struct_type, structs, vec![5, 0, 4, 1, 3, 2], 0, 6)?;
        let lists = admitted_list(list_type.clone(), &[0, 1, 3, 4, 6], structs, query)?;
        let lists = multi_chunk_dictionary_slice(list_type.clone(), lists, vec![3, 0, 2, 1], 1, 3)?;
        Ok((list_type, lists))
    }

    fn direct_list_from_scalar_rows(
        list_type: &DataType,
        rows: &[Value],
        query: &QueryContext,
    ) -> Result<Vector> {
        let DataType::Nested(metadata) = list_type else {
            unreachable!()
        };
        let NestedType::List(struct_type) = metadata.as_ref() else {
            unreachable!()
        };
        let mut offsets = Vec::with_capacity(rows.len() + 1);
        let mut strings = Vec::new();
        offsets.push(0);
        for row in rows {
            let Value::Nested(row) = row else {
                return Err(Error::Internal("test LIST scalar".into()));
            };
            let NestedPayload::Sequence(structs) = &row.payload else {
                return Err(Error::Internal("test LIST payload".into()));
            };
            for value in structs {
                let Value::Nested(value) = value else {
                    return Err(Error::Internal("test STRUCT scalar".into()));
                };
                let NestedPayload::Struct(fields) = &value.payload else {
                    return Err(Error::Internal("test STRUCT payload".into()));
                };
                let Some(Value::Varchar(value)) = fields.first() else {
                    return Err(Error::Internal("test VARCHAR field".into()));
                };
                strings.push(Value::Varchar(value.clone()));
            }
            offsets.push(strings.len());
        }
        let structs = admitted_struct(
            struct_type.clone(),
            vec![Vector::flat(DataType::Varchar, strings)?],
            query,
        )?;
        admitted_list(list_type.clone(), &offsets, structs, query)
    }

    #[test]
    fn prepared_comparison_resolves_dictionary_chunks_and_slices_once() -> Result<()> {
        let query = QueryContext::background();
        let (data_type, left) = nested_list_vector(["a", "b", "c", "d"], &query)?;
        let (_, right) = nested_list_vector(["a", "b", "c", "e"], &query)?;
        let left = left.slice(0, 2)?;
        let right = right.slice(0, 2)?;
        let DataType::Nested(metadata) = &data_type else {
            unreachable!()
        };
        let NestedType::List(child) = metadata.as_ref() else {
            unreachable!()
        };
        let children = vec![TypeRegistry::builtins().bind(child)?];
        let prepared = prepare_nested_comparison(metadata, &children, &left, &right, &query)?
            .expect("supported recursive physical plan");
        assert!(
            prepared
                .arena
                .chunk_roots
                .iter()
                .all(|&root| root != usize::MAX)
        );
        assert_eq!(prepared.compare_top(0, &query)?, Some(Ordering::Less));
        assert_eq!(prepared.compare_top(1, &query)?, Some(Ordering::Equal));

        let bound = TypeRegistry::builtins().bind(&data_type)?;
        assert_eq!(
            bound.compare_batch(&left, &right, &query)?,
            vec![Some(Ordering::Less), Some(Ordering::Equal)]
        );
        Ok(())
    }

    #[test]
    fn prepared_comparison_composes_nonzero_offsets_against_scalar_oracle() -> Result<()> {
        let query = QueryContext::background();
        let (data_type, left) = nested_offset_list_vector(&query)?;
        let scalar_rows = left.values().collect::<Vec<_>>();
        let right = direct_list_from_scalar_rows(&data_type, &scalar_rows, &query)?;
        let bound = TypeRegistry::builtins().bind(&data_type)?;
        let expected = scalar_rows
            .iter()
            .zip(right.values())
            .map(|(left, right)| bound.compare(left, &right, &query).map(Some))
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(bound.compare_batch(&left, &right, &query)?, expected);

        let DataType::Nested(metadata) = &data_type else {
            unreachable!()
        };
        let NestedType::List(child) = metadata.as_ref() else {
            unreachable!()
        };
        let children = vec![TypeRegistry::builtins().bind(child)?];
        let prepared = prepare_nested_comparison(metadata, &children, &left, &right, &query)?
            .expect("supported recursive physical plan");
        assert!(
            prepared
                .arena
                .chunk_roots
                .iter()
                .all(|&root| root != usize::MAX)
        );
        for (index, expected) in expected.into_iter().enumerate() {
            assert_eq!(prepared.compare_top(index, &query)?, expected);
        }
        Ok(())
    }

    #[test]
    fn prepared_comparison_declines_large_pair_graph_before_allocation() -> Result<()> {
        let query = QueryContext::background();
        let data_type = NestedType::Struct(vec![("v".into(), DataType::Varchar)]).data_type();
        let mut chunks = Vec::new();
        for _ in 0..9 {
            chunks.push(admitted_struct(
                data_type.clone(),
                vec![Vector::flat(
                    DataType::Varchar,
                    vec![Value::Varchar("same".into())],
                )?],
                &query,
            )?);
        }
        let left = Vector::chunked(data_type.clone(), chunks.clone())?;
        let right = Vector::chunked(data_type.clone(), chunks)?;
        let DataType::Nested(metadata) = &data_type else {
            unreachable!()
        };
        let NestedType::Struct(fields) = metadata.as_ref() else {
            unreachable!()
        };
        let children = fields
            .iter()
            .map(|(_, field)| TypeRegistry::builtins().bind(field))
            .collect::<Result<Vec<_>>>()?;
        assert!(prepare_nested_comparison(metadata, &children, &left, &right, &query)?.is_none());
        assert_eq!(
            TypeRegistry::builtins()
                .bind(&data_type)?
                .compare_batch(&left, &right, &query)?,
            vec![Some(Ordering::Equal); 9]
        );
        Ok(())
    }

    #[test]
    fn prepared_comparison_propagates_admission_and_cancellation() -> Result<()> {
        let construction = QueryContext::background();
        let (data_type, left) = nested_list_vector(["a", "b", "c", "d"], &construction)?;
        let (_, right) = nested_list_vector(["a", "b", "c", "e"], &construction)?;
        let DataType::Nested(metadata) = &data_type else {
            unreachable!()
        };
        let NestedType::List(child) = metadata.as_ref() else {
            unreachable!()
        };
        let children = vec![TypeRegistry::builtins().bind(child)?];

        let pool = Arc::new(MemoryPool::default());
        pool.publish_limit(Some(1))?;
        let limited = QueryContext::background().with_memory_pool(pool.clone());
        assert!(matches!(
            prepare_nested_comparison(metadata, &children, &left, &right, &limited),
            Err(Error::Resource(_))
        ));
        assert_eq!(pool.used()?, 0);

        let prepared =
            prepare_nested_comparison(metadata, &children, &left, &right, &construction)?
                .expect("supported recursive physical plan");
        let interrupt = InterruptHandle::default();
        let interrupted = QueryContext::new(interrupt.clone(), None, 1, 1)?;
        interrupt.interrupt();
        assert!(matches!(
            prepared.compare_top(0, &interrupted),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}
