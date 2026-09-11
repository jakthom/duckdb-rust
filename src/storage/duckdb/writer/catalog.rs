use super::{Encoder, Result, TableDefinition};
use crate::catalog::TypeDefinition;

const CATALOG_NAME: &str = "duckdb_rust";

/// Serialize DuckDB's `CreateTypeInfo` payload. Storage 69 moved catalog
/// identities to a single qualified-name field; older releases retain the
/// catalog and schema as separate strings.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn type_definition(
    output: &mut Encoder,
    definition: &TypeDefinition,
    version: u64,
) -> Result<()> {
    definition.validate()?;
    if definition.name.schema.is_empty() || definition.name.name.is_empty() {
        return Err(crate::Error::InvalidInput(
            "native named type requires a schema and name".into(),
        ));
    }
    output.property(100, 8); // TYPE_ENTRY
    if version < 69 {
        output.field(101);
        output.string(CATALOG_NAME)?;
        output.field(102);
        output.string(&definition.name.schema)?;
    }
    output.property(105, 0); // ERROR_ON_CONFLICT
    if version >= 69 {
        output.field(111);
        output.property(100, 3);
        output.string(CATALOG_NAME)?;
        output.string(&definition.name.schema)?;
        output.string(&definition.name.name)?;
        output.end();
    }
    output.field(200);
    output.string(&definition.name.name)?;
    output.field(201);
    super::super::primitive::write_type(output, &definition.data_type)?;
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn table_definition(
    output: &mut Encoder,
    table: &TableDefinition,
    version: u64,
    context: &crate::parallel::QueryContext,
) -> Result<()> {
    output.property(100, 1);
    // Development replay/checkpoint binding navigates the schema components
    // between catalog and table. The catalog is descriptive, not an attachment
    // target; readers rebind the table in the opened database. Retain a complete
    // three-part name even though Snapshot does not own a filesystem basename.
    output.field(101);
    output.string("duckdb_rust")?;
    output.field(102);
    output.string(&table.name.schema)?;
    output.property(105, 0);
    output.field(200);
    output.string(&table.name.name)?;
    output.field(201);
    output.property(100, table.columns.len() as u64);
    for column in &table.columns {
        column_definition(output, column, version, context)?;
    }
    output.end();
    let constraints: Vec<_> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.nullable)
        .collect();
    if !constraints.is_empty() || !table.unique_keys.is_empty() {
        output.property(202, (constraints.len() + table.unique_keys.len()) as u64);
        for (index, _) in constraints {
            output.boolean(true);
            output.property(100, 1);
            output.property(200, index as u64);
            output.end();
        }
        for key in &table.unique_keys {
            output.boolean(true);
            output.property(100, 3);
            output.field(200);
            output.boolean(key.primary);
            output.property(201, u64::MAX);
            output.property(202, key.columns.len() as u64);
            for &column in &key.columns {
                output.string(&table.columns[column].name)?;
            }
            output.end();
        }
    }
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn column_definition(
    output: &mut Encoder,
    column: &crate::catalog::ColumnDefinition,
    version: u64,
    context: &crate::parallel::QueryContext,
) -> Result<()> {
    output.field(100);
    output.string(&column.name)?;
    output.field(101);
    super::super::primitive::write_type(output, &column.data_type)?;
    if let Some(default) = &column.default {
        output.field(102);
        output.boolean(true);
        super::super::parsed::write(output, default, version, context)?;
    }
    output.property(103, 0);
    output.property(104, 0);
    output.end();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DataType,
        catalog::{TypeDefinition, TypeName},
        storage::duckdb::{binary::Reader, catalog},
    };

    fn definition() -> TypeDefinition {
        TypeDefinition::enumeration(
            TypeName::new("s", "mood"),
            ["sad", "ok", "happy"].map(str::to_owned).into(),
        )
        .unwrap()
    }

    fn encoded(version: u64) -> Result<Vec<u8>> {
        let mut output = Encoder::default();
        type_definition(&mut output, &definition(), version)?;
        Ok(output.0)
    }

    fn decode(bytes: Vec<u8>) -> Result<TypeDefinition> {
        let mut reader = Reader::new(bytes);
        let qualified = catalog::create_base(&mut reader, 8)?;
        let result = catalog::type_definition_at(&mut reader, qualified)?;
        if !reader.finished() {
            return Err(crate::Error::Corrupt("trailing named-type metadata".into()));
        }
        Ok(result)
    }

    #[test]
    fn named_enum_create_info_matches_release_and_development_layouts() -> Result<()> {
        let release = [
            100, 0, 8, 101, 0, 11, b'd', b'u', b'c', b'k', b'd', b'b', b'_', b'r', b'u', b's',
            b't', 102, 0, 1, b's', 105, 0, 0, 200, 0, 4, b'm', b'o', b'o', b'd', 201, 0, 100, 0,
            104, 101, 0, 1, 100, 0, 6, 200, 0, 3, 201, 0, 3, 3, b's', b'a', b'd', 2, b'o', b'k', 5,
            b'h', b'a', b'p', b'p', b'y', 255, 255, 255, 255, 255, 255,
        ];
        let development = [
            100, 0, 8, 105, 0, 0, 111, 0, 100, 0, 3, 11, b'd', b'u', b'c', b'k', b'd', b'b', b'_',
            b'r', b'u', b's', b't', 1, b's', 4, b'm', b'o', b'o', b'd', 255, 255, 200, 0, 4, b'm',
            b'o', b'o', b'd', 201, 0, 100, 0, 104, 101, 0, 1, 100, 0, 6, 200, 0, 3, 201, 0, 3, 3,
            b's', b'a', b'd', 2, b'o', b'k', 5, b'h', b'a', b'p', b'p', b'y', 255, 255, 255, 255,
            255, 255,
        ];
        assert_eq!(encoded(68)?, release);
        assert_eq!(encoded(69)?, development);
        assert_eq!(decode(release.into())?, definition());
        assert_eq!(decode(development.into())?, definition());
        assert_eq!(
            definition().data_type,
            DataType::enumeration(vec!["sad".into(), "ok".into(), "happy".into()])?
        );
        Ok(())
    }

    #[test]
    fn named_enum_reader_rejects_every_truncated_create_info() -> Result<()> {
        for version in [68, 69] {
            let bytes = encoded(version)?;
            for end in 0..bytes.len() {
                assert!(
                    decode(bytes[..end].to_vec()).is_err(),
                    "v{version} end {end}"
                );
            }
            assert_eq!(decode(bytes)?, definition());
        }
        Ok(())
    }

    #[test]
    fn enum_metadata_rejects_aliases_and_oversized_allocation_before_labels() -> Result<()> {
        let wire = |alias: Option<&str>, count: usize| -> Result<Vec<u8>> {
            let mut output = Encoder::default();
            output.property(100, 104);
            output.field(101);
            output.boolean(true);
            output.property(100, 6);
            if let Some(alias) = alias {
                output.field(101);
                output.string(alias)?;
            }
            output.property(200, count as u64);
            output.property(201, count as u64);
            output.end();
            output.end();
            Ok(output.0)
        };

        assert!(matches!(
            catalog::logical_type(&mut Reader::new(wire(Some("mood"), 0)?)),
            Err(crate::Error::Unsupported(_))
        ));
        let count = crate::storage::duckdb::primitive::ENUM_METADATA_BUDGET
            / crate::storage::duckdb::primitive::ENUM_LABEL_OVERHEAD
            + 1;
        assert!(matches!(
            catalog::logical_type(&mut Reader::new(wire(None, count)?)),
            Err(crate::Error::Resource(_))
        ));
        Ok(())
    }
}
