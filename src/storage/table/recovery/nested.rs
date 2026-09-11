//! Transaction-private physical children keep validity separate from payload.
//! A NULL logical parent can receive child writes before its validity record;
//! UNION tags and inactive members can be temporarily inconsistent in the log.
use super::*;
use crate::common::{DataType, NestedPayload, NestedType, NestedValue, Value};

const MAX_NODES: usize = 16_777_216;

struct Node {
    data_type: DataType,
    valid: bool,
    payload: Payload,
}
enum Payload {
    Scalar(Value),
    Children(Vec<Node>),
}

pub(super) struct Pending {
    nodes: BTreeMap<(TableName, usize, RowId), Node>,
    remaining: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Pending {
    pub(super) fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            remaining: MAX_NODES,
        }
    }

    fn node(
        &mut self,
        snapshot: &Snapshot,
        table: &TableName,
        column: usize,
        id: RowId,
        context: &QueryContext,
    ) -> Result<&mut Node> {
        let key = (table.clone(), column, id);
        if !self.nodes.contains_key(&key) {
            let table = snapshot.get(table)?;
            let data_type = &table
                .definition
                .columns
                .get(column)
                .ok_or_else(|| corrupt("physical update column out of bounds"))?
                .data_type;
            let row = table
                .rows
                .get(&id)
                .ok_or_else(|| corrupt("physical update row out of bounds"))?;
            let value = row
                .get(column)
                .ok_or_else(|| corrupt("physical update column out of bounds"))?;
            let node = Node::new(data_type, value, 0, &mut self.remaining, context)?;
            self.nodes.insert(key.clone(), node);
        }
        self.nodes
            .get_mut(&key)
            .ok_or_else(|| corrupt("missing staged physical row"))
    }

    pub(super) fn update(
        &mut self,
        snapshot: &Snapshot,
        table: &TableName,
        column: usize,
        path: &[usize],
        values: &[(RowId, Value)],
        context: &QueryContext,
    ) -> Result<()> {
        for (id, value) in values {
            let node = self
                .node(snapshot, table, column, *id, context)?
                .select(path, context)?;
            if !matches!(node.payload, Payload::Scalar(_)) {
                return Err(corrupt("physical value update targets a container"));
            }
            context
                .types()
                .bind(&node.data_type)?
                .validate(value, context)?;
            node.payload = Payload::Scalar(value.clone());
        }
        Ok(())
    }

    pub(super) fn validity(
        &mut self,
        snapshot: &Snapshot,
        table: &TableName,
        column: usize,
        path: &[usize],
        values: &[(RowId, bool)],
        context: &QueryContext,
    ) -> Result<()> {
        for (id, valid) in values {
            self.node(snapshot, table, column, *id, context)?
                .select(path, context)?
                .valid = *valid;
        }
        Ok(())
    }

    pub(super) fn discard_table(&mut self, table: &TableName) {
        self.nodes.retain(|(name, _, _), _| name != table);
    }

    pub(super) fn finish(&mut self, snapshot: &mut Snapshot, context: &QueryContext) -> Result<()> {
        for ((name, column, id), node) in std::mem::take(&mut self.nodes) {
            context.check()?;
            let table = snapshot.recovery_table(&name)?;
            if let Some(row) = table.rows.get_mut(&id) {
                let value = node.materialize(0, context)?;
                context
                    .types()
                    .bind(&node.data_type)?
                    .validate(&value, context)?;
                *row.get_mut(column)
                    .ok_or_else(|| corrupt("physical update column disappeared"))? = value;
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Node {
    fn new(
        data_type: &DataType,
        value: &Value,
        depth: usize,
        remaining: &mut usize,
        context: &QueryContext,
    ) -> Result<Self> {
        context.check()?;
        if depth > 64 {
            return Err(Error::Resource(
                "physical recovery nesting exceeds 64".into(),
            ));
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| Error::Resource("physical recovery exceeds 16 million nodes".into()))?;
        let fields = match data_type {
            DataType::Nested(metadata) => match metadata.as_ref() {
                NestedType::Struct(fields) => {
                    Some(fields.iter().map(|(_, ty)| ty.clone()).collect::<Vec<_>>())
                }
                NestedType::Tuple(fields) => Some(fields.clone()),
                NestedType::Union(fields) => Some(
                    std::iter::once(DataType::UTinyInt)
                        .chain(fields.iter().map(|(_, ty)| ty.clone()))
                        .collect(),
                ),
                _ => None,
            },
            _ => None,
        };
        let payload = if let Some(fields) = fields {
            let mut children = Vec::with_capacity(fields.len());
            for (index, ty) in fields.iter().enumerate() {
                let value = match value {
                    Value::Null => Value::Null,
                    Value::Nested(nested) => match &nested.payload {
                        NestedPayload::Struct(values) => values
                            .get(index)
                            .cloned()
                            .ok_or_else(|| corrupt("physical STRUCT arity"))?,
                        NestedPayload::Union { tag, value } => {
                            if index == 0 {
                                Value::Unsigned(*tag as u128)
                            } else if index == tag + 1 {
                                value.clone()
                            } else {
                                Value::Null
                            }
                        }
                        _ => return Err(corrupt("physical nested payload shape")),
                    },
                    _ => return Err(corrupt("physical nested value shape")),
                };
                children.push(Node::new(ty, &value, depth + 1, remaining, context)?);
            }
            Payload::Children(children)
        } else {
            Payload::Scalar(value.clone())
        };
        Ok(Self {
            data_type: data_type.clone(),
            valid: !value.is_null(),
            payload,
        })
    }

    fn select(&mut self, path: &[usize], context: &QueryContext) -> Result<&mut Self> {
        if path.len() > 64 {
            return Err(Error::Resource("physical recovery path exceeds 64".into()));
        }
        let mut node = self;
        for index in path {
            context.check()?;
            let Payload::Children(children) = &mut node.payload else {
                return Err(corrupt("physical recovery path enters scalar"));
            };
            node = children
                .get_mut(*index)
                .ok_or_else(|| corrupt("physical recovery child index out of bounds"))?;
        }
        Ok(node)
    }

    fn materialize(&self, depth: usize, context: &QueryContext) -> Result<Value> {
        context.check()?;
        if depth > 64 {
            return Err(Error::Resource(
                "physical recovery nesting exceeds 64".into(),
            ));
        }
        if !self.valid {
            return Ok(Value::Null);
        }
        match &self.payload {
            Payload::Scalar(value) => {
                if value.is_null() {
                    return Err(corrupt(
                        "physical recovery makes NULL valid without a value",
                    ));
                }
                Ok(value.clone())
            }
            Payload::Children(children) => {
                let DataType::Nested(metadata) = &self.data_type else {
                    return Err(corrupt("physical child metadata"));
                };
                let values = children
                    .iter()
                    .map(|child| child.materialize(depth + 1, context))
                    .collect::<Result<Vec<_>>>()?;
                let payload = if matches!(metadata.as_ref(), NestedType::Union(_)) {
                    let Some(Value::Unsigned(tag)) = values.first() else {
                        return Err(corrupt("physical UNION invalid tag"));
                    };
                    let tag = usize::try_from(*tag)
                        .map_err(|_| corrupt("physical UNION tag overflow"))?;
                    let value = values
                        .get(tag + 1)
                        .ok_or_else(|| corrupt("physical UNION tag out of range"))?
                        .clone();
                    if values
                        .iter()
                        .enumerate()
                        .skip(1)
                        .any(|(index, value)| index != tag + 1 && !value.is_null())
                    {
                        return Err(corrupt("physical UNION inactive member is non-NULL"));
                    }
                    NestedPayload::Union { tag, value }
                } else {
                    NestedPayload::Struct(values)
                };
                NestedValue::value(self.data_type.clone(), payload)
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn corrupt(message: &str) -> Error {
    Error::Corrupt(message.into())
}
