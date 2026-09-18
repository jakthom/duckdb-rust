//! Development-only instrumentation. Production cfg removes the attribute itself.
use proc_macro::TokenStream;
use quote::{ToTokens, quote, quote_spanned};
use syn::{Item, ReturnType, Type, parse_macro_input, spanned::Spanned, visit_mut::VisitMut};

/// Establish a SQL scope outside the ordinary operation instrumentation.
#[proc_macro_attribute]
pub fn statement(arguments: TokenStream, input: TokenStream) -> TokenStream {
    let arguments = parse_macro_input!(arguments with syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated);
    let mut function = parse_macro_input!(input as syn::ItemFn);
    if !(2..=3).contains(&arguments.len()) {
        return syn::Error::new_spanned(
            arguments,
            "statement expects SQL, phase, and optionally an output callback",
        )
        .to_compile_error()
        .into();
    }
    let sql = &arguments[0];
    let phase = &arguments[1];
    let output = arguments
        .get(2)
        .map(|callback| quote!(#callback))
        .unwrap_or_else(|| quote!(|_| {}));
    let body = &function.block;
    function.block = syn::parse_quote!({
        ::duckdb_dev::statement::run(&(#sql), #phase, || #body, #output)
    });
    quote!(#function).into()
}

#[proc_macro_attribute]
pub fn instrument(arguments: TokenStream, input: TokenStream) -> TokenStream {
    if !arguments.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "instrument takes no arguments",
        )
        .to_compile_error()
        .into();
    }
    let mut item = parse_macro_input!(input as Item);
    let result = match &mut item {
        Item::Fn(function) => instrument_function(&function.sig, &mut function.block, ""),
        Item::Impl(implementation) => {
            let scope = match &implementation.trait_ {
                Some((_, name, _)) => format!(
                    "{} as {}",
                    implementation.self_ty.to_token_stream(),
                    name.to_token_stream()
                ),
                None => implementation.self_ty.to_token_stream().to_string(),
            };
            implementation.items.iter_mut().try_for_each(|item| {
                if let syn::ImplItem::Fn(method) = item {
                    instrument_function(&method.sig, &mut method.block, &scope)?;
                }
                Ok(())
            })
        }
        Item::Trait(interface) => {
            let scope = interface.ident.to_string();
            interface.items.iter_mut().try_for_each(|item| {
                if let syn::TraitItem::Fn(method) = item
                    && let Some(body) = &mut method.default
                {
                    instrument_function(&method.sig, body, &scope)?;
                }
                Ok(())
            })
        }
        other => Err(syn::Error::new_spanned(
            other,
            "instrument applies to functions, impls, and traits",
        )),
    };
    match result {
        Ok(()) => quote!(#item).into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn result_type(output: &ReturnType) -> Option<&Type> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    let Type::Path(path) = ty.as_ref() else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != "Result" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

fn scalar(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => scalar(&reference.elem),
        Type::Path(path) => path.path.segments.last().is_some_and(|part| {
            matches!(
                part.ident.to_string().as_str(),
                "bool"
                    | "str"
                    | "String"
                    | "usize"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "f32"
                    | "f64"
            )
        }),
        _ => false,
    }
}

fn instrument_function(
    signature: &syn::Signature,
    body: &mut syn::Block,
    scope: &str,
) -> syn::Result<()> {
    // Rust cannot emit runtime events during const evaluation. Keep the const contract intact.
    if signature.constness.is_some() {
        return Ok(());
    }
    if signature.asyncness.is_some() {
        return Err(syn::Error::new_spanned(
            signature,
            "async operations require poll-scoped instrumentation; an entered span must not cross await",
        ));
    }
    let name = &signature.ident;
    let operation = if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}::{name}")
    };
    let output_type = signature.output.to_token_stream().to_string();
    let arguments = signature.inputs.iter().filter_map(|argument| {
        let syn::FnArg::Typed(argument) = argument else { return None };
        let syn::Pat::Ident(pattern) = argument.pat.as_ref() else { return None };
        let name = &pattern.ident;
        let field = name.to_string();
        if scalar(&argument.ty) {
            Some(quote!(::duckdb_dev::value(#field, &#name);))
        } else if matches!(argument.ty.as_ref(), Type::Reference(reference) if matches!(reference.elem.as_ref(), Type::Slice(_))) {
            let field = format!("{field}.len");
            Some(quote!(::duckdb_dev::value(#field, &#name.len());))
        } else { None }
    }).collect::<Vec<_>>();
    let entry = quote_spanned! {name.span()=>
        #[allow(unused_imports)]
        use ::duckdb_dev::FinishCall as _;
        ::duckdb_dev::init();
        let __duckdb_dev_operation = ::duckdb_dev::Operation::enter(
            ::duckdb_dev::tracing::span!(
                ::duckdb_dev::tracing::Level::TRACE,
                #operation,
                outcome = ::duckdb_dev::tracing::field::Empty,
                error = ::duckdb_dev::tracing::field::Empty,
                return_type = #output_type,
            )
        );
        #(#arguments)*
    };
    let mut original = body.clone();
    let mut nested = NestedFunctions {
        scope: &operation,
        error: None,
    };
    nested.visit_block_mut(&mut original);
    if let Some(error) = nested.error {
        return Err(error);
    }
    let returns_result = matches!(&signature.output, ReturnType::Type(_, ty)
        if matches!(ty.as_ref(), Type::Path(path) if path.path.segments.last().is_some_and(|part| part.ident == "Result")));
    let completion =
        if returns_result {
            let value = result_type(&signature.output).is_some_and(scalar).then(|| quote! {
            if let Ok(value) = &__duckdb_dev_result { ::duckdb_dev::value("return", value); }
        });
            let ReturnType::Type(_, output) = &signature.output else {
                unreachable!()
            };
            quote! {
                let __duckdb_dev_result = ::duckdb_dev::call(move || -> #output #original);
                __duckdb_dev_operation.result(&__duckdb_dev_result);
                #value
                __duckdb_dev_result
            }
        } else if matches!(&signature.output, ReturnType::Type(_, ty) if scalar(ty)) {
            quote! {
                let __duckdb_dev_result = ::duckdb_dev::call(move || #original);
                ::duckdb_dev::value("return", &__duckdb_dev_result);
                __duckdb_dev_result
            }
        } else {
            // Preserve borrowing and drop order for arbitrary returned handles and iterators.
            quote!(#original)
        };
    *body = syn::parse2(quote_spanned! {body.span()=> { #entry #completion }})?;
    Ok(())
}

struct NestedFunctions<'a> {
    scope: &'a str,
    error: Option<syn::Error>,
}
impl VisitMut for NestedFunctions<'_> {
    fn visit_item_fn_mut(&mut self, function: &mut syn::ItemFn) {
        if let Err(error) = instrument_function(&function.sig, &mut function.block, self.scope) {
            self.error = Some(error);
        }
    }
    fn visit_item_impl_mut(&mut self, implementation: &mut syn::ItemImpl) {
        let scope = match &implementation.trait_ {
            Some((_, name, _)) => format!(
                "{}::{} as {}",
                self.scope,
                implementation.self_ty.to_token_stream(),
                name.to_token_stream()
            ),
            None => format!(
                "{}::{}",
                self.scope,
                implementation.self_ty.to_token_stream()
            ),
        };
        for item in &mut implementation.items {
            if let syn::ImplItem::Fn(method) = item
                && let Err(error) = instrument_function(&method.sig, &mut method.block, &scope)
            {
                self.error = Some(error);
            }
        }
    }
    fn visit_item_trait_mut(&mut self, interface: &mut syn::ItemTrait) {
        let scope = format!("{}::{}", self.scope, interface.ident);
        for item in &mut interface.items {
            if let syn::TraitItem::Fn(method) = item
                && let Some(body) = &mut method.default
                && let Err(error) = instrument_function(&method.sig, body, &scope)
            {
                self.error = Some(error);
            }
        }
    }
    fn visit_type_mut(&mut self, _: &mut Type) {}
    fn visit_item_const_mut(&mut self, _: &mut syn::ItemConst) {}
    fn visit_item_static_mut(&mut self, _: &mut syn::ItemStatic) {}
    fn visit_expr_mut(&mut self, expression: &mut syn::Expr) {
        if matches!(expression, syn::Expr::Const(_)) {
            return;
        }
        let label = match expression {
            syn::Expr::MethodCall(call) => Some(format!("call::{}", call.method)),
            syn::Expr::Call(call) => match call.func.as_ref() {
                // Constructors are coercion sites, not independently executing
                // routines. Retain contextual coercions through Result/Option.
                syn::Expr::Path(path)
                    if path.path.segments.last().is_some_and(|part| {
                        part.ident
                            .to_string()
                            .chars()
                            .next()
                            .is_some_and(char::is_uppercase)
                    }) =>
                {
                    None
                }
                syn::Expr::Path(path) => Some(format!("call::{}", path.path.to_token_stream())),
                _ => Some("call::callable".into()),
            },
            _ => None,
        };
        let terminates = matches!(expression, syn::Expr::Call(call) if matches!(call.func.as_ref(),
            syn::Expr::Path(path) if matches!(path.path.to_token_stream().to_string().as_str(),
                "std :: process :: exit" | "std :: process :: abort")));
        let span = match expression {
            syn::Expr::MethodCall(call) => call.method.span(),
            syn::Expr::Call(call) => match call.func.as_ref() {
                syn::Expr::Path(path) => path
                    .path
                    .segments
                    .last()
                    .map(|part| part.ident.span())
                    .unwrap_or_else(|| expression.span()),
                _ => expression.span(),
            },
            _ => expression.span(),
        };
        syn::visit_mut::visit_expr_mut(self, expression);
        if let Some(label) = label {
            let original = expression.clone();
            let start = quote_spanned! {span=> ::duckdb_dev::Operation::enter(
                ::duckdb_dev::tracing::span!(::duckdb_dev::tracing::Level::TRACE, #label,
                    outcome = ::duckdb_dev::tracing::field::Empty)
            )};
            let wrapped = if terminates {
                quote_spanned! {span=> ({
                    let __duckdb_dev_call = #start;
                    ::duckdb_dev::flush();
                    #original
                })}
            } else {
                quote_spanned! {span=> (#start, #original).__duckdb_dev_finish_call()}
            };
            match syn::parse2(wrapped) {
                Ok(wrapped) => *expression = wrapped,
                Err(error) => self.error = Some(error),
            }
        }
    }
}
