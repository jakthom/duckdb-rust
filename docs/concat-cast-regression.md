# Selected string casts in concat

Pinned development `99063af2bd` selects VARCHAR arguments for ordinary scalar
`concat`, including BLOB and non-sequence nested inputs. Its
`src/function/scalar/string/concat.cpp` binds argument casts before string
concatenation. Untyped NULL inputs are skipped; all-NULL scalar concat returns
an empty VARCHAR, while a zero-argument call fails binding.

Rust previously called diagnostic `Display` directly. That bypassed retained
cast adapters and let `concat(make_timestamp(-9223372036854775806))` produce a
successful fallback string even though development reports fatal INTERNAL.
The scalar adapter now requests explicit selected VARCHAR casts through the
shared argument-cast-mode contract. Execution only accepts validated VARCHAR
or NULL arguments and checks cancellation and allocation failure while appending.
No temporal-specific error or formatting logic lives in concat.

The new component regression passes with both scalar evaluators and both
optimizers. It checks selected replacement casts, NULL inputs, typed parameters,
fatal temporal/STRUCT conversion under CAST and TRY_CAST, failed multirow UPDATE
atomicity and rollback. Combined local suites pass BIGNUM (3), binary scalars
(5), contracts (26), temporal (20), and nested (21); workspace check and clippy
pass. Instrumentation inventory is 282 files, 2,523 functions and 208 interface
methods with no missing attributes. Exhaustive trace compilation passes in
50.56 seconds with no errors and deletes temporary telemetry. The integration
lead owns the combined maintained Kani
checkpoint under the exploratory policy; this internal fix is not a subsystem
completion or performance claim.

LIST/ARRAY input selects a separate concat overload in development. This scalar
commit explicitly rejects that not-yet-integrated specialization; the nested
worker is delivering its binding/evaluation helper for the same checkpoint.
That helper must select before VARCHAR conversion and remove the provisional
guard and its negative assertion. It must not duplicate the scalar catalog name
or stringify sequence inputs. Operator `||` and named arithmetic aliases remain
separate paths and are not broadened by this patch.
