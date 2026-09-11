use std::collections::{BTreeMap, BTreeSet};

use crate::common::{Error, Result};

use super::{DropBehavior, ObjectIdentity};

const MAX_DEPENDENCY_OBJECTS: usize = 1_000_000;
const MAX_DEPENDENCY_EDGES: usize = 4_000_000;

/// Flags stored with the object that depends on a subject. A dependency with
/// no flags is automatic: dropping its subject also drops the dependent even
/// under RESTRICT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DependentFlags {
    pub blocking: bool,
    pub owned_by: bool,
    pub alter_blocking: bool,
}

impl DependentFlags {
    pub const fn automatic() -> Self {
        Self {
            blocking: false,
            owned_by: false,
            alter_blocking: false,
        }
    }

    pub const fn blocking() -> Self {
        Self {
            blocking: true,
            ..Self::automatic()
        }
    }

    pub const fn owned_by() -> Self {
        Self {
            owned_by: true,
            ..Self::automatic()
        }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            blocking: self.blocking || other.blocking,
            owned_by: self.owned_by || other.owned_by,
            alter_blocking: self.alter_blocking || other.alter_blocking,
        }
    }
}

/// Flags stored with the subject of an edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubjectFlags {
    pub ownership: bool,
}

impl SubjectFlags {
    pub const fn ordinary() -> Self {
        Self { ownership: false }
    }

    pub const fn ownership() -> Self {
        Self { ownership: true }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            ownership: self.ownership || other.ownership,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DependencyEdge {
    dependent: DependentFlags,
    subject: SubjectFlags,
}

impl DependencyEdge {
    fn merge(self, other: Self) -> Self {
        Self {
            dependent: self.dependent.merge(other.dependent),
            subject: self.subject.merge(other.subject),
        }
    }

    fn validate(self) -> Result<()> {
        if self.dependent.owned_by != self.subject.ownership {
            return Err(Error::InvalidInput(
                "ownership dependencies require matching owned_by and ownership flags".into(),
            ));
        }
        Ok(())
    }
}

type Adjacency = BTreeMap<ObjectIdentity, BTreeMap<ObjectIdentity, DependencyEdge>>;

/// A deterministic, bidirectional dependency index. Edges point from a
/// dependent to the subject it requires. Mutations validate a cloned graph and
/// replace the original only after both indexes agree exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencyGraph {
    subjects_by_dependent: Adjacency,
    dependents_by_subject: Adjacency,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.subjects_by_dependent.is_empty() && self.dependents_by_subject.is_empty()
    }

    pub fn edge_count(&self) -> usize {
        self.subjects_by_dependent.values().map(BTreeMap::len).sum()
    }

    /// Return the exact flags for one checked edge.
    pub fn dependency_flags(
        &self,
        dependent: ObjectIdentity,
        subject: ObjectIdentity,
    ) -> Result<Option<(DependentFlags, SubjectFlags)>> {
        self.validate()?;
        validate_endpoints(dependent, subject)?;
        Ok(self
            .subjects_by_dependent
            .get(&dependent)
            .and_then(|subjects| subjects.get(&subject))
            .map(|edge| (edge.dependent, edge.subject)))
    }

    /// Check that every graph endpoint is owned by the surrounding catalog.
    /// The graph cannot establish this invariant without that catalog.
    pub fn validate_object_set(
        &self,
        mut contains: impl FnMut(ObjectIdentity) -> bool,
    ) -> Result<()> {
        self.validate()?;
        for (dependent, subjects) in &self.subjects_by_dependent {
            if !contains(*dependent) || subjects.keys().any(|subject| !contains(*subject)) {
                return Err(Error::Internal(
                    "dependency graph references an absent catalog object".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn add_dependency(
        &mut self,
        dependent: ObjectIdentity,
        subject: ObjectIdentity,
        dependent_flags: DependentFlags,
        subject_flags: SubjectFlags,
    ) -> Result<()> {
        self.validate()?;
        validate_endpoints(dependent, subject)?;
        let incoming = DependencyEdge {
            dependent: dependent_flags,
            subject: subject_flags,
        };
        incoming.validate()?;

        let mut candidate = self.clone();
        let edge = candidate
            .subjects_by_dependent
            .get(&dependent)
            .and_then(|subjects| subjects.get(&subject))
            .copied()
            .map_or(incoming, |existing| existing.merge(incoming));
        edge.validate()?;
        candidate
            .subjects_by_dependent
            .entry(dependent)
            .or_default()
            .insert(subject, edge);
        candidate
            .dependents_by_subject
            .entry(subject)
            .or_default()
            .insert(dependent, edge);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn remove_dependency(
        &mut self,
        dependent: ObjectIdentity,
        subject: ObjectIdentity,
    ) -> Result<bool> {
        self.validate()?;
        validate_endpoints(dependent, subject)?;
        let mut candidate = self.clone();
        let removed = remove_edge(&mut candidate.subjects_by_dependent, dependent, subject);
        let reverse_removed = remove_edge(&mut candidate.dependents_by_subject, subject, dependent);
        if removed != reverse_removed {
            return Err(Error::Internal(
                "dependency reverse indexes disagree during removal".into(),
            ));
        }
        candidate.validate()?;
        *self = candidate;
        Ok(removed)
    }

    /// Removes every edge to or from an object. The graph is unchanged if its
    /// reverse indexes are malformed or validation otherwise fails.
    pub fn remove_object(&mut self, object: ObjectIdentity) -> Result<bool> {
        self.validate()?;
        let mut candidate = self.clone();
        let subjects = candidate
            .subjects_by_dependent
            .get(&object)
            .map(|edges| edges.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let dependents = candidate
            .dependents_by_subject
            .get(&object)
            .map(|edges| edges.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let existed = !subjects.is_empty() || !dependents.is_empty();

        for subject in subjects {
            let forward = remove_edge(&mut candidate.subjects_by_dependent, object, subject);
            let reverse = remove_edge(&mut candidate.dependents_by_subject, subject, object);
            if !forward || !reverse {
                return Err(Error::Internal(
                    "dependency reverse indexes disagree during object removal".into(),
                ));
            }
        }
        for dependent in dependents {
            let forward = remove_edge(&mut candidate.subjects_by_dependent, dependent, object);
            let reverse = remove_edge(&mut candidate.dependents_by_subject, object, dependent);
            if !forward || !reverse {
                return Err(Error::Internal(
                    "dependency reverse indexes disagree during object removal".into(),
                ));
            }
        }
        candidate.validate()?;
        *self = candidate;
        Ok(existed)
    }

    /// Returns all objects removed by a DROP, ordered so every dependent is
    /// removed before its subject. Under RESTRICT, automatic dependencies and
    /// owned subjects are still removed; blocking and owned-by dependents fail.
    pub fn plan_drop(
        &self,
        root: ObjectIdentity,
        behavior: DropBehavior,
    ) -> Result<Vec<ObjectIdentity>> {
        self.validate()?;
        let closure = self.drop_closure(root, behavior)?;
        self.order_dependents_first(&closure)
    }

    pub fn alter_blockers(&self, subject: ObjectIdentity) -> Result<Vec<ObjectIdentity>> {
        self.validate()?;
        Ok(self
            .dependents_by_subject
            .get(&subject)
            .into_iter()
            .flat_map(|dependents| dependents.iter())
            .filter_map(|(dependent, edge)| edge.dependent.alter_blocking.then_some(*dependent))
            .collect())
    }

    pub fn ensure_can_alter(&self, subject: ObjectIdentity) -> Result<()> {
        let blockers = self.alter_blockers(subject)?;
        if blockers.is_empty() {
            return Ok(());
        }
        Err(Error::Catalog(format!(
            "cannot alter {subject} because {} dependenc{} block the alteration",
            blockers.len(),
            if blockers.len() == 1 { "y" } else { "ies" }
        )))
    }

    /// Checks exact equality of both indexes and all ownership invariants.
    pub fn validate(&self) -> Result<()> {
        let forward_edges = checked_edge_count(&self.subjects_by_dependent)?;
        let reverse_edges = checked_edge_count(&self.dependents_by_subject)?;
        if forward_edges != reverse_edges {
            return Err(Error::Internal(
                "dependency reverse indexes have different edge counts".into(),
            ));
        }
        if forward_edges > MAX_DEPENDENCY_EDGES {
            return Err(Error::Resource(
                "dependency graph edge limit exceeded".into(),
            ));
        }

        let mut objects = BTreeSet::new();
        let mut ownership_parent = BTreeMap::new();
        for (dependent, subjects) in &self.subjects_by_dependent {
            if subjects.is_empty() {
                return Err(Error::Internal(
                    "dependency graph contains an empty forward index".into(),
                ));
            }
            objects.insert(*dependent);
            for (subject, edge) in subjects {
                validate_endpoints(*dependent, *subject)?;
                edge.validate()?;
                objects.insert(*subject);
                let reverse = self
                    .dependents_by_subject
                    .get(subject)
                    .and_then(|dependents| dependents.get(dependent));
                if reverse != Some(edge) {
                    return Err(Error::Internal(
                        "dependency reverse index is missing or has different flags".into(),
                    ));
                }
                if edge.subject.ownership
                    && ownership_parent
                        .insert(*subject, *dependent)
                        .is_some_and(|owner| owner != *dependent)
                {
                    return Err(Error::Catalog(format!(
                        "{subject} cannot be owned by more than one object"
                    )));
                }
            }
        }
        for (subject, dependents) in &self.dependents_by_subject {
            if dependents.is_empty() {
                return Err(Error::Internal(
                    "dependency graph contains an empty reverse index".into(),
                ));
            }
            for (dependent, edge) in dependents {
                let forward = self
                    .subjects_by_dependent
                    .get(dependent)
                    .and_then(|subjects| subjects.get(subject));
                if forward != Some(edge) {
                    return Err(Error::Internal(
                        "dependency forward index is missing or has different flags".into(),
                    ));
                }
            }
        }
        if objects.len() > MAX_DEPENDENCY_OBJECTS {
            return Err(Error::Resource(
                "dependency graph object limit exceeded".into(),
            ));
        }
        validate_ownership_acyclic(&self.subjects_by_dependent, &objects)
    }

    fn drop_closure(
        &self,
        root: ObjectIdentity,
        behavior: DropBehavior,
    ) -> Result<BTreeSet<ObjectIdentity>> {
        let mut closure = BTreeSet::from([root]);
        let mut pending = vec![root];
        let mut blockers = BTreeSet::new();
        while let Some(object) = pending.pop() {
            if let Some(dependents) = self.dependents_by_subject.get(&object) {
                for (dependent, edge) in dependents {
                    let automatic = !edge.dependent.blocking && !edge.dependent.owned_by;
                    if behavior == DropBehavior::Restrict && !automatic {
                        blockers.insert((object, *dependent));
                        continue;
                    }
                    if closure.insert(*dependent) {
                        pending.push(*dependent);
                    }
                }
            }
            if let Some(subjects) = self.subjects_by_dependent.get(&object) {
                for (subject, edge) in subjects {
                    if edge.subject.ownership && closure.insert(*subject) {
                        pending.push(*subject);
                    }
                }
            }
            if closure.len() > MAX_DEPENDENCY_OBJECTS {
                return Err(Error::Resource(
                    "dependency drop plan object limit exceeded".into(),
                ));
            }
        }
        if let Some((subject, dependent)) = blockers
            .into_iter()
            .find(|(_, dependent)| !closure.contains(dependent))
        {
            return Err(Error::Catalog(format!(
                "cannot drop {subject} because {dependent} depends on it"
            )));
        }
        Ok(closure)
    }

    fn order_dependents_first(
        &self,
        closure: &BTreeSet<ObjectIdentity>,
    ) -> Result<Vec<ObjectIdentity>> {
        let mut incoming = closure
            .iter()
            .copied()
            .map(|object| (object, 0usize))
            .collect::<BTreeMap<_, _>>();
        for (dependent, subjects) in &self.subjects_by_dependent {
            if !closure.contains(dependent) {
                continue;
            }
            for subject in subjects.keys().filter(|subject| closure.contains(subject)) {
                let count = incoming
                    .get_mut(subject)
                    .ok_or_else(|| Error::Internal("drop plan lost a dependency subject".into()))?;
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("dependency drop ordering overflow".into()))?;
            }
        }

        let mut ready = incoming
            .iter()
            .filter_map(|(object, count)| (*count == 0).then_some(*object))
            .collect::<BTreeSet<_>>();
        let mut ordered = Vec::with_capacity(closure.len());
        while let Some(object) = ready.pop_first() {
            ordered.push(object);
            if let Some(subjects) = self.subjects_by_dependent.get(&object) {
                for subject in subjects.keys().filter(|subject| closure.contains(subject)) {
                    let count = incoming.get_mut(subject).ok_or_else(|| {
                        Error::Internal("drop plan lost a dependency subject".into())
                    })?;
                    *count = count.checked_sub(1).ok_or_else(|| {
                        Error::Internal("dependency drop ordering underflow".into())
                    })?;
                    if *count == 0 {
                        ready.insert(*subject);
                    }
                }
            }
        }
        if ordered.len() != closure.len() {
            return Err(Error::Catalog(
                "cannot produce a dependent-before-subject drop order for a dependency cycle"
                    .into(),
            ));
        }
        Ok(ordered)
    }
}

fn validate_endpoints(dependent: ObjectIdentity, subject: ObjectIdentity) -> Result<()> {
    if dependent == subject {
        return Err(Error::InvalidInput(
            "an object cannot depend on itself".into(),
        ));
    }
    if dependent.catalog != subject.catalog {
        return Err(Error::InvalidInput(
            "cross-catalog dependencies are not supported".into(),
        ));
    }
    Ok(())
}

fn remove_edge(index: &mut Adjacency, first: ObjectIdentity, second: ObjectIdentity) -> bool {
    let Some(edges) = index.get_mut(&first) else {
        return false;
    };
    let removed = edges.remove(&second).is_some();
    if edges.is_empty() {
        index.remove(&first);
    }
    removed
}

fn checked_edge_count(index: &Adjacency) -> Result<usize> {
    index.values().try_fold(0usize, |count, edges| {
        count
            .checked_add(edges.len())
            .ok_or_else(|| Error::Resource("dependency graph edge count overflow".into()))
    })
}

fn validate_ownership_acyclic(
    subjects: &Adjacency,
    objects: &BTreeSet<ObjectIdentity>,
) -> Result<()> {
    let mut incoming = objects
        .iter()
        .copied()
        .map(|object| (object, 0usize))
        .collect::<BTreeMap<_, _>>();
    for edges in subjects.values() {
        for (subject, edge) in edges {
            if edge.subject.ownership {
                *incoming
                    .get_mut(subject)
                    .ok_or_else(|| Error::Internal("ownership graph lost a subject".into()))? += 1;
            }
        }
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(object, count)| (*count == 0).then_some(*object))
        .collect::<BTreeSet<_>>();
    let mut visited = 0usize;
    while let Some(owner) = ready.pop_first() {
        visited += 1;
        if let Some(edges) = subjects.get(&owner) {
            for (subject, edge) in edges {
                if !edge.subject.ownership {
                    continue;
                }
                let count = incoming
                    .get_mut(subject)
                    .ok_or_else(|| Error::Internal("ownership graph lost a subject".into()))?;
                *count -= 1;
                if *count == 0 {
                    ready.insert(*subject);
                }
            }
        }
    }
    if visited != objects.len() {
        return Err(Error::Catalog(
            "ownership dependencies cannot contain a cycle".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogId, CatalogIdentity, CatalogObjectKind, CatalogVersion, ObjectId};

    fn catalog() -> CatalogIdentity {
        CatalogIdentity::new(CatalogId::allocate().unwrap(), Some(CatalogVersion::new(1)))
    }

    fn object(catalog: CatalogIdentity, kind: CatalogObjectKind) -> ObjectIdentity {
        ObjectIdentity::new(catalog.id, ObjectId::allocate().unwrap(), kind)
    }

    fn table(catalog: CatalogIdentity) -> ObjectIdentity {
        object(catalog, CatalogObjectKind::Table)
    }

    #[test]
    fn diamond_drop_is_deterministic_and_dependents_first() {
        let catalog = catalog();
        let root = table(catalog);
        let left = table(catalog);
        let right = table(catalog);
        let leaf = table(catalog);
        let mut graph = DependencyGraph::new();
        for (dependent, subject) in [(left, root), (right, root), (leaf, left), (leaf, right)] {
            graph
                .add_dependency(
                    dependent,
                    subject,
                    DependentFlags::automatic(),
                    SubjectFlags::ordinary(),
                )
                .unwrap();
        }

        assert_eq!(
            graph.plan_drop(root, DropBehavior::Restrict).unwrap(),
            vec![leaf, left, right, root]
        );
        assert_eq!(
            graph.plan_drop(root, DropBehavior::Cascade).unwrap(),
            vec![leaf, left, right, root]
        );
    }

    #[test]
    fn restrict_blocks_regular_edges_but_removes_automatic_dependents() {
        let catalog = catalog();
        let subject = table(catalog);
        let automatic = table(catalog);
        let blocker = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                automatic,
                subject,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        graph
            .add_dependency(
                blocker,
                subject,
                DependentFlags::blocking(),
                SubjectFlags::ordinary(),
            )
            .unwrap();

        assert!(matches!(
            graph.plan_drop(subject, DropBehavior::Restrict),
            Err(Error::Catalog(_))
        ));
        assert_eq!(
            graph.plan_drop(subject, DropBehavior::Cascade).unwrap(),
            vec![automatic, blocker, subject]
        );
    }

    #[test]
    fn ownership_drops_owned_subject_and_blocks_direct_subject_drop() {
        let catalog = catalog();
        let owner = table(catalog);
        let owned = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                owner,
                owned,
                DependentFlags::owned_by(),
                SubjectFlags::ownership(),
            )
            .unwrap();

        assert_eq!(
            graph.plan_drop(owner, DropBehavior::Restrict).unwrap(),
            vec![owner, owned]
        );
        assert!(matches!(
            graph.plan_drop(owned, DropBehavior::Restrict),
            Err(Error::Catalog(_))
        ));
        assert_eq!(
            graph.plan_drop(owned, DropBehavior::Cascade).unwrap(),
            vec![owner, owned]
        );
    }

    #[test]
    fn restrict_accepts_a_blocker_already_in_the_automatic_closure() {
        let catalog = catalog();
        let root = table(catalog);
        let intermediate = table(catalog);
        let dependent = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                intermediate,
                root,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        graph
            .add_dependency(
                dependent,
                intermediate,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        graph
            .add_dependency(
                dependent,
                root,
                DependentFlags::blocking(),
                SubjectFlags::ordinary(),
            )
            .unwrap();

        assert_eq!(
            graph.plan_drop(root, DropBehavior::Restrict).unwrap(),
            vec![dependent, intermediate, root]
        );
    }

    #[test]
    fn ownership_cycles_and_multiple_owners_are_rejected_atomically() {
        let catalog = catalog();
        let first = table(catalog);
        let second = table(catalog);
        let third = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                first,
                second,
                DependentFlags::owned_by(),
                SubjectFlags::ownership(),
            )
            .unwrap();
        let before = graph.clone();
        assert!(matches!(
            graph.add_dependency(
                second,
                first,
                DependentFlags::owned_by(),
                SubjectFlags::ownership()
            ),
            Err(Error::Catalog(_))
        ));
        assert_eq!(graph, before);
        assert!(matches!(
            graph.add_dependency(
                third,
                second,
                DependentFlags::owned_by(),
                SubjectFlags::ownership()
            ),
            Err(Error::Catalog(_))
        ));
        assert_eq!(graph, before);
    }

    #[test]
    fn ordinary_cycle_is_stored_but_cannot_claim_a_strict_drop_order() {
        let catalog = catalog();
        let first = table(catalog);
        let second = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                first,
                second,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        graph
            .add_dependency(
                second,
                first,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        assert!(matches!(
            graph.plan_drop(first, DropBehavior::Cascade),
            Err(Error::Catalog(_))
        ));
    }

    #[test]
    fn alter_blocking_is_independent_of_drop_blocking() {
        let catalog = catalog();
        let subject = table(catalog);
        let dependent = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                dependent,
                subject,
                DependentFlags {
                    alter_blocking: true,
                    ..DependentFlags::automatic()
                },
                SubjectFlags::ordinary(),
            )
            .unwrap();

        assert_eq!(graph.alter_blockers(subject).unwrap(), vec![dependent]);
        assert!(matches!(
            graph.ensure_can_alter(subject),
            Err(Error::Catalog(_))
        ));
        assert_eq!(
            graph.plan_drop(subject, DropBehavior::Restrict).unwrap(),
            vec![dependent, subject]
        );
    }

    #[test]
    fn checked_add_rejects_self_cross_catalog_and_unpaired_ownership() {
        let first_catalog = catalog();
        let second_catalog = catalog();
        let first = table(first_catalog);
        let other_catalog = table(second_catalog);
        let mut graph = DependencyGraph::new();
        for result in [
            graph.add_dependency(
                first,
                first,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            ),
            graph.add_dependency(
                first,
                other_catalog,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            ),
            graph.add_dependency(
                first,
                table(first_catalog),
                DependentFlags::owned_by(),
                SubjectFlags::ordinary(),
            ),
        ] {
            assert!(matches!(result, Err(Error::InvalidInput(_))));
            assert!(graph.is_empty());
        }
    }

    #[test]
    fn object_kind_is_part_of_the_key_without_restricting_valid_edges() {
        let catalog = catalog();
        let schema = object(catalog, CatalogObjectKind::Schema);
        let table = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                table,
                schema,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        assert_eq!(
            graph.plan_drop(schema, DropBehavior::Restrict).unwrap(),
            vec![table, schema]
        );
    }

    #[test]
    fn dependencies_survive_unrelated_catalog_version_increments() {
        let catalog_v1 = catalog();
        let subject_v1 = table(catalog_v1);
        let dependent_v1 = table(catalog_v1);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                dependent_v1,
                subject_v1,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();

        let catalog_v2 = CatalogIdentity::new(
            catalog_v1.id,
            Some(catalog_v1.version.unwrap().checked_next().unwrap()),
        );
        let subject_v2 = ObjectIdentity::new(catalog_v2.id, subject_v1.object, subject_v1.kind);
        let dependent_v2 =
            ObjectIdentity::new(catalog_v2.id, dependent_v1.object, dependent_v1.kind);
        assert_eq!(subject_v1, subject_v2);
        assert_eq!(dependent_v1, dependent_v2);
        assert_eq!(
            graph.plan_drop(subject_v2, DropBehavior::Restrict).unwrap(),
            vec![dependent_v2, subject_v2]
        );
    }

    #[test]
    fn duplicate_add_merges_flags_and_remove_cleans_both_indexes() {
        let catalog = catalog();
        let subject = table(catalog);
        let dependent = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                dependent,
                subject,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        graph
            .add_dependency(
                dependent,
                subject,
                DependentFlags {
                    blocking: true,
                    alter_blocking: true,
                    ..DependentFlags::automatic()
                },
                SubjectFlags::ordinary(),
            )
            .unwrap();
        assert_eq!(graph.edge_count(), 1);
        assert!(matches!(
            graph.plan_drop(subject, DropBehavior::Restrict),
            Err(Error::Catalog(_))
        ));
        assert_eq!(graph.alter_blockers(subject).unwrap(), vec![dependent]);
        assert!(graph.remove_dependency(dependent, subject).unwrap());
        assert!(graph.is_empty());
        assert!(!graph.remove_dependency(dependent, subject).unwrap());
    }

    #[test]
    fn malformed_reverse_index_is_detected_and_mutations_roll_back() {
        let catalog = catalog();
        let subject = table(catalog);
        let dependent = table(catalog);
        let mut graph = DependencyGraph::new();
        graph
            .add_dependency(
                dependent,
                subject,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        graph
            .dependents_by_subject
            .get_mut(&subject)
            .unwrap()
            .insert(
                dependent,
                DependencyEdge {
                    dependent: DependentFlags::blocking(),
                    subject: SubjectFlags::ordinary(),
                },
            );
        assert!(matches!(graph.validate(), Err(Error::Internal(_))));

        let malformed = graph.clone();
        assert!(matches!(
            graph.remove_dependency(dependent, subject),
            Err(Error::Internal(_))
        ));
        assert_eq!(graph, malformed);
        assert!(matches!(
            graph.remove_object(subject),
            Err(Error::Internal(_))
        ));
        assert_eq!(graph, malformed);
    }
}
