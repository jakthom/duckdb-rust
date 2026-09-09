use super::*;

/// Append-only access to one canonical key component. The caller owns the
/// reusable allocation; an adapter cannot inspect or change earlier components.
/// Each append is fallible and the component is limited to 16 MiB.
pub struct KeyWriter<'a> {
    output: &'a mut Vec<u8>,
    start: usize,
    failure: Option<&'static str>,
}

impl KeyWriter<'_> {
    pub fn push(&mut self, byte: u8) -> Result<()> {
        self.extend_from_slice(&[byte])
    }
    pub fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<()> {
        if self.failure.is_none() {
            self.failure = if bytes.len() > 16 * 1024 * 1024 - (self.output.len() - self.start) {
                Some("type key exceeds 16 MiB")
            } else if self.output.try_reserve(bytes.len()).is_err() {
                Some("cannot allocate type key")
            } else {
                None
            };
        }
        if let Some(message) = self.failure {
            return Err(Error::Resource(message.into()));
        }
        self.output.extend_from_slice(bytes);
        Ok(())
    }
}

impl BoundType {
    /// Append one self-delimiting component with the selected type adapter.
    /// Failure leaves all existing bytes intact, including on a partial adapter
    /// write or cancellation. Reusing the output avoids per-value allocations.
    pub fn append_key(
        &self,
        value: &Value,
        output: &mut Vec<u8>,
        context: &QueryContext,
    ) -> Result<()> {
        self.validate(value, context)?;
        self.append_validated_key(value, output, context)
    }

    /// Visit canonical non-NULL keys in column order; NULL values produce None.
    /// The column is validated before the first callback. Key bytes borrow a
    /// reusable buffer and live only for that callback; callers copy explicitly
    /// if they retain them. Errors or cancellation stop visitation immediately.
    pub fn for_each_key(
        &self,
        input: &super::super::vector::Vector,
        context: &QueryContext,
        mut visit: impl FnMut(usize, Option<&[u8]>) -> Result<()>,
    ) -> Result<()> {
        self.validate_vector(input, context)?;
        let mut key = Vec::new();
        for (index, value) in input.values().enumerate() {
            context.check()?;
            if value.is_null() {
                visit(index, None)?;
            } else {
                key.clear();
                self.append_validated_key(value, &mut key, context)?;
                visit(index, Some(&key))?;
            }
        }
        context.check()
    }

    fn append_validated_key(
        &self,
        value: &Value,
        output: &mut Vec<u8>,
        context: &QueryContext,
    ) -> Result<()> {
        let prefix = output.len();
        output
            .try_reserve(if value.is_null() { 1 } else { 9 })
            .map_err(|_| Error::Resource("cannot allocate composite type key".into()))?;
        if value.is_null() {
            output.push(0);
            return Ok(());
        }
        output.push(1);
        output.extend_from_slice(&[0; 8]);
        let start = output.len();
        let mut writer = KeyWriter {
            output,
            start,
            failure: None,
        };
        let result = self
            .adapter
            .write_key(&self.data_type, value, &mut writer, context);
        let result = writer
            .failure
            .map_or(result, |message| Err(Error::Resource(message.into())));
        if let Err(error) = context.check().and(result) {
            output.truncate(prefix);
            return Err(error);
        }
        let length = (output.len() - start) as u64;
        output[prefix + 1..start].copy_from_slice(&length.to_le_bytes());
        Ok(())
    }
}
