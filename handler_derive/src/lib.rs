//! `#[handler]` — write tail-call dispatch handlers and their slow
//! paths against a single receiver (`frame: Frame`) instead of the raw
//! multi-argument `rust-preserve-none` signature.
//!
//! # Dispatch handlers: `#[handler(Add)]`
//!
//! ```ignore
//! #[handler(Add)]
//! fn op_add(frame: Frame) -> Acc {
//!     let lhs = frame.regs().get(frame.signed::<0>(), frame.heap());
//!     ...
//!     slow!(slow_add)
//! }
//! ```
//!
//! expands to
//!
//! ```ignore
//! #[rustfmt::skip]
//! #[rustc_align(32)]
//! extern "rust-preserve-none" fn op_add<'h, const STRIDE: usize>(
//!     ip: *const u8, regs: Regs, acc: Acc, ctx: &mut Ctx<'_, 'h>,
//!     table: TableArg<'h>, roots: RootsArg<'h>, float: FloatReg,
//! ) -> Acc { /* body with receiver calls and exit macros expanded */ }
//! ```
//!
//! # Trampolines: bare `#[handler]`
//!
//! ```ignore
//! #[handler]
//! #[cold]
//! #[inline(never)]
//! fn throw_dispatch(frame: Frame) -> Acc { ... }
//! ```
//!
//! the full dispatch ABI and stride without an opcode: hand-written
//! attributes (`#[cold]`, `#[rustc_align(32)]`, ...) pass through.
//! Trampolines may dispatch (`dispatch!`, `dispatch_frame!`) and
//! decode operands, but not use
//! the size-computing exits (`next!`, `reenter!` need
//! the opcode).
//!
//! # Slow paths
//!
//! A slow path is the same `#[handler(Opcode)]` form plus `#[cold]`
//! and not listed in `handler_table!`. The hot handler tail-calls it
//! with `tail!(path)`; the slow path owns its continuation — it
//! computes a result and re-enters with `set_acc` +
//! `reenter!` (keeping the advance compile-time via the opcode).
//!
//! # Receiver syntax
//!
//! - `frame.method(..)` — the closed set of reads and writes over the
//!   generated parameters (see the `macro_rules!` shims and the arms
//!   of `expand_method`): `ip`, `regs`, `acc`, `float`,
//!   `pc`, `ctx`, `signed::<I>()`, `unsigned::<I>()`,
//!   `opcode_check()`, and the writes `set_acc*` / `set_float`
//!   (dispatch handlers only). Everything else on `Ctx` goes through
//!   `frame.ctx().heap()` and friends — an explicit hop, since
//!   `frame.ctx()` is the `ctx` parameter itself.
//! - `next!()` / `dispatch!(p)` / `slow!(path)` / `reenter!(pc)` /
//!   `dispatch_frame!(pc)` — control transfers that
//!   never return. The
//!   rewrite turns the short form into a call of the module-level
//!   `macro_rules!` (`dispatch!`, `next!`, `reenter!`, `slow!`,
//!   `dispatch_frame!`) defined by the consumer
//!   next to the handlers, filling in the opcode and the parameters:
//!   `next!()` becomes `next!(Add, ip, regs, ctx, table, roots, float,
//!   acc)`. The macro bodies (including their `star_fusion` cfg
//!   blocks, resolved against the consumer's features) are therefore
//!   written once, commented, in the interpreter crate.
//!
//! `tail!(path)` instantiates the target at the handler's
//! stride (`path::<STRIDE>`); do not write the turbofish yourself.
//!
//! The receiver may use any name that does not collide with a
//! generated parameter (`ip`, `regs`, `acc`, `ctx`, `table`, `roots`,
//! `float`, `STRIDE`); `frame` is the convention. The handwritten
//! signature is pure documentation; it is never compiled. The macro
//! never binds the receiver: every `frame.method(..)` call is
//! rewritten into an expression over the generated parameters, so any
//! other use of the receiver fails with E0425. Bodies must not shadow
//! the receiver, the parameter names, or the handler's dependencies
//! (`Ops`, `Opcode`, `Acc`, `FloatReg`, `Regs`, `ThreadState`,
//! `star_lookahead`, `throw_dispatch`). Rewriting is token-level (no
//! re-parsing), which keeps `become` — an unstable keyword syn cannot
//! express — usable in the consumer's macro bodies. The rewritten
//! tokens keep the source spans of the written name, `!`, argument
//! group, and method identifier wherever they survive, so IDEs can
//! map call sites onto the expansion.

use proc_macro::TokenStream;
use proc_macro2::{
    Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream as TokenStream2, TokenTree,
};
use quote::quote;
use syn::Token;
use syn::parse_macro_input;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;

const EXITS: &[&str] = &[
    "next",
    "dispatch",
    "tail",
    "reenter",
    "dispatch_frame",
    "call_enter",
];

/// `handler_table!(op_load, op_store, ..)` — dispatch table generation.
#[proc_macro]
pub fn handler_table(input: TokenStream) -> TokenStream {
    let names = parse_macro_input!(input with Punctuated::<Ident, Token![,]>::parse_terminated);
    match expand_table(&names) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_table(names: &Punctuated<Ident, Token![,]>) -> syn::Result<TokenStream2> {
    let mut narrow = Vec::new();
    let mut wide = Vec::new();
    for name in names {
        let op = opcode_of(name)?;
        narrow.push(quote!(t[Opcode::#op as usize] = #name::<1> as Handler;));
        wide.push(quote!(t[Opcode::#op as usize] = #name::<2> as Handler;));
    }
    Ok(quote! {
        #[rustfmt::skip]
        const fn __table_narrow() -> [Handler; 256] {
            let mut t = [op_trap::<1> as Handler; 256];
            #(#narrow)*
            t
        }

        #[rustfmt::skip]
        const fn __table_wide() -> [Handler; 256] {
            let mut t = [op_trap::<2> as Handler; 256];
            #(#wide)*
            t
        }
    })
}

/// `op_jump_if_truthy` -> `Opcode::JumpIfTruthy`.
fn opcode_of(name: &Ident) -> syn::Result<Ident> {
    let full = name.to_string();
    let Some(rest) = full.strip_prefix("op_") else {
        return Err(syn::Error::new(
            name.span(),
            "handler names must start with `op_` (they map to the like-named `Opcode` variant)",
        ));
    };
    if rest.is_empty() {
        return Err(syn::Error::new(name.span(), "handler name is just `op_`"));
    }
    let mut camel = String::new();
    for seg in rest.split('_') {
        let mut chars = seg.chars();
        if let Some(f) = chars.next() {
            camel.extend(f.to_uppercase());
        }
        camel.push_str(chars.as_str());
    }
    Ok(Ident::new(&camel, name.span()))
}

const PARAM_NAMES: &[&str] = &[
    "ip", "regs", "acc", "ctx", "table", "roots", "float", "STRIDE",
];

/// The two handler forms; every form has the full dispatch ABI and
/// the stride. What a form may use follows from the one thing it may
/// not know: the opcode (instruction-size math).
enum HandlerForm {
    /// `#[handler(Add)]`: opcode known. Hot handlers are listed in
    /// `handler_table!`; cold ones (`#[cold]`) are tail-called through
    /// `tail!` and own their continuation (`set_acc` + `reenter!`).
    Dispatch(Ident),
    /// `#[handler]`: no opcode (trampolines such as `op_trap`,
    /// `throw_dispatch`, a future `op_wide`). May dispatch and decode
    /// operands, but not use the size-computing exits.
    Trampoline,
}

impl HandlerForm {
    fn op(&self) -> Option<&Ident> {
        match self {
            HandlerForm::Dispatch(op) => Some(op),
            _ => None,
        }
    }
}

#[proc_macro_attribute]
pub fn handler(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func: syn::ItemFn = parse_macro_input!(item as syn::ItemFn);
    let form = if attr.is_empty() {
        HandlerForm::Trampoline
    } else {
        match syn::parse::<Ident>(attr) {
            Ok(op) => HandlerForm::Dispatch(op),
            Err(_) => return syn::Error::new(
                Span::call_site(),
                "#[handler] takes either no argument (dispatch trampoline) or an Opcode variant",
            )
            .to_compile_error()
            .into(),
        }
    };
    match expand(form, &func) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(form: HandlerForm, func: &syn::ItemFn) -> syn::Result<TokenStream2> {
    let (recv, ret) = validate_sig(func)?;
    let name = &func.sig.ident;
    let vis = &func.vis;
    let block = &func.block;
    let attrs = &func.attrs;
    let mut rw = Rewriter {
        op: form.op().cloned(),
        recv: recv.to_string(),
        mut_acc: false,
        mut_float: false,
    };
    let body = rw.rewrite(quote!(#block));
    let acc = if rw.mut_acc {
        quote!(mut acc)
    } else {
        quote!(acc)
    };
    let float = if rw.mut_float {
        quote!(mut float)
    } else {
        quote!(float)
    };
    // every generated fn is 32-byte aligned; the handler form governs
    // only the rewriting — the emission is one shape
    Ok(quote! {
        #(#attrs)*
        #[rustfmt::skip]
        #[rustc_align(32)]
        #vis extern "rust-preserve-none" fn #name<'h, const STRIDE: usize>(
            ip: *const u8,
            regs: Regs,
            #acc: Acc,
            ctx: &mut Ctx<'_, 'h>,
            table: TableArg<'h>,
            roots: RootsArg<'h>,
            #float: FloatReg,
        ) #ret {
            #body
        }
    })
}

/// The handwritten signature is `fn <name>(<recv>: Frame)` (the
/// `-> Acc` return type is optional — the macro generates it): the
/// receiver may have any name that does not collide with a generated
/// parameter. It is discarded, so anything fancier is rejected to
/// keep the source form greppable.
fn validate_sig(func: &syn::ItemFn) -> syn::Result<(Ident, TokenStream2)> {
    let sig = &func.sig;
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            sig.generics.span(),
            "#[handler] fns take no generics",
        ));
    }
    if sig.inputs.len() != 1 {
        return Err(syn::Error::new(
            sig.span(),
            "#[handler] fns take exactly one `<name>: Frame` parameter",
        ));
    }
    let syn::FnArg::Typed(pat) = &sig.inputs[0] else {
        return Err(syn::Error::new(sig.span(), "#[handler] fns have no `self`"));
    };
    let syn::Pat::Ident(pi) = &*pat.pat else {
        return Err(syn::Error::new(
            pat.span(),
            "the #[handler] receiver must be a plain identifier",
        ));
    };
    let recv = &pi.ident;
    if PARAM_NAMES.contains(&recv.to_string().as_str()) {
        return Err(syn::Error::new(
            recv.span(),
            format!("receiver name `{recv}` collides with a generated parameter"),
        ));
    }
    if !matches!(&*pat.ty, syn::Type::Path(tp) if tp.path.is_ident("Frame")) {
        return Err(syn::Error::new(
            pat.span(),
            "#[handler] takes a single `<name>: Frame` parameter",
        ));
    }
    // the return type is optional (`Acc`); `-> !` is also accepted
    // for fns whose every path must transfer control away
    let ret = match &sig.output {
        syn::ReturnType::Default => quote!(-> Acc),
        syn::ReturnType::Type(_, ty) if matches!(&**ty, syn::Type::Never(_)) => quote!(-> !),
        syn::ReturnType::Type(_, ty) if matches!(&**ty, syn::Type::Path(tp) if tp.path.is_ident("Acc")) =>
        {
            quote!(-> Acc)
        }
        syn::ReturnType::Type(_, ty) => {
            return Err(syn::Error::new(ty.span(), "#[handler] fns return `Acc`"));
        }
    };
    Ok((recv.clone(), ret))
}

struct Rewriter {
    /// `Some(opcode)` when the handler knows its instruction; slow
    /// paths and trampolines decode operands from the instruction.
    op: Option<Ident>,
    recv: String,
    mut_acc: bool,
    mut_float: bool,
}

impl Rewriter {
    fn rewrite(&mut self, tokens: TokenStream2) -> TokenStream2 {
        let tts: Vec<TokenTree> = tokens.into_iter().collect();
        let mut out: Vec<TokenTree> = Vec::with_capacity(tts.len());
        let mut i = 0;
        while i < tts.len() {
            if let Some((method, turbo, args, group, next)) = match_method(&tts, i, &self.recv) {
                let args = self.rewrite(args);
                // reads and writes expand to documented `macro_rules!`
                // like the exits; the remaining rewrites are plain
                // expressions. Both re-span the emitted call so IDEs
                // can map it onto the written `frame.m(..)`.
                let bang = method.span();
                match self.expand_method(&method, turbo, args) {
                    Ok(ts) => out.extend(respan_exit(ts, bang, group)),
                    Err(err) => out.extend(err.to_compile_error()),
                }
                i = next;
            } else if let Some((name, bang, args, group, next)) = match_exit(&tts, i) {
                let args = self.rewrite(args);
                match self.expand_exit(&name, args) {
                    Ok(ts) => out.extend(respan_exit(ts, bang, group)),
                    Err(err) => out.extend(err.to_compile_error()),
                }
                i = next;
            } else {
                match &tts[i] {
                    TokenTree::Group(g) => {
                        let mut ng = Group::new(g.delimiter(), self.rewrite(g.stream()));
                        ng.set_span(g.span());
                        out.push(TokenTree::Group(ng));
                    }
                    t => out.push(t.clone()),
                }
                i += 1;
            }
        }
        out.into_iter().collect()
    }

    // ---- exit macros ---------------------------------------------------

    /// Expand the short exit form into a call of the module-level
    /// macro with the opcode and parameters filled in. The emitted
    /// call re-uses the written name, `!`, and argument group, so IDEs
    /// can map the call site onto the expansion and resolve it to the
    /// (documented) `macro_rules!` definition.
    fn expand_exit(&mut self, name: &Ident, args: TokenStream2) -> syn::Result<TokenStream2> {
        // the size-computing exits need the opcode
        if matches!(name.to_string().as_str(), "next" | "reenter") && self.op.is_none() {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "`{name}!` advances past the instruction, which needs the opcode: use #[handler(Opcode)]"
                ),
            ));
        }
        let op = self.op.as_ref();
        match name.to_string().as_str() {
            "next" => {
                no_args(name, &args)?;
                let op = op.unwrap();
                Ok(quote!(#name!(#op, ip, regs, ctx, table, roots, float, acc)))
            }
            "dispatch" => {
                let p = one_arg(name, args)?;
                Ok(quote!(#name!(#p, acc, regs, ctx, table, roots, float)))
            }
            "tail" => {
                let path = one_arg(name, args)?;
                reject_turbofish(name, &path)?;
                Ok(quote!(#name!(ip, regs, acc, ctx, table, roots, float, #path::<STRIDE>)))
            }
            "reenter" => {
                let pc = one_arg(name, args)?;
                let op = op.unwrap();
                Ok(quote!(#name!(#op, #pc, regs, acc, ctx, table, roots, float)))
            }
            "dispatch_frame" => {
                let pc = one_arg(name, args)?;
                Ok(quote!(#name!(#pc, acc, ctx, table, roots, float)))
            }
            "call_enter" => {
                if self.op.is_none() {
                    return Err(syn::Error::new(
                        name.span(),
                        "`call_enter!` advances past the instruction, which needs the opcode: \
                         use #[handler(Opcode)]",
                    ));
                }
                let op = self.op.as_ref().unwrap();
                let (result, pc) = match split_args(args).as_slice() {
                    [r, p] if !r.is_empty() && !p.is_empty() => (r.clone(), p.clone()),
                    _ => {
                        return Err(syn::Error::new(
                            name.span(),
                            "`call_enter(result, pc)` takes exactly two arguments",
                        ));
                    }
                };
                Ok(quote!(
                    #name!(#op, #result, #pc, regs, acc, ctx, table, roots, float)
                ))
            }
            _ => unreachable!("match_exit only yields known exit names"),
        }
    }

    // ---- frame.method() reads and writes ------------------------------------

    fn expand_method(
        &mut self,
        method: &Ident,
        turbo: Option<TokenStream2>,
        args: TokenStream2,
    ) -> syn::Result<TokenStream2> {
        let op = &self.op;
        match method.to_string().as_str() {
            // zero-argument reads and one-argument writes: expand to
            // the documented `macro_rules!` defined next to the exits,
            // with the generated parameters passed explicitly
            // (macro hygiene hides locals)
            "ip" => no_args(method, &args).map(|_| quote!(#method!(ip))),
            "regs" => no_args(method, &args).map(|_| quote!(#method!(regs))),
            "acc" => no_args(method, &args).map(|_| quote!(#method!(acc, ctx))),
            "float" => no_args(method, &args).map(|_| quote!(#method!(float))),
            "pc" => no_args(method, &args).map(|_| quote!(#method!(ip, ctx))),
            "ctx" => no_args(method, &args).map(|_| quote!(ctx)),
            "ctx_ref" => no_args(method, &args).map(|_| quote!(&*ctx)),

            // opcode-known handlers only
            "opcode_check" => {
                if self.op.is_none() {
                    return Err(syn::Error::new(
                        method.span(),
                        "`opcode_check` asserts the instruction against a known opcode: use #[handler(Opcode)]",
                    ));
                }
                no_args(method, &args).map(|_| quote!(Ops::<STRIDE>::new(ip, Opcode::#op)))
            }

            // accumulator / float writes
            "set_acc" | "set_acc_smi" => {
                self.mut_acc = true;
                one_arg(method, args).map(|a| quote!(#method!(acc, #a)))
            }
            "set_float" => {
                self.mut_float = true;
                one_arg(method, args).map(|a| quote!(#method!(float, #a)))
            }

            // operand decode: the opcode-known form can assert the
            // instruction; slow paths and trampolines read it from the
            // instruction.
            "signed" | "unsigned" => {
                no_args(method, &args)?;
                let n = const_arg(method, turbo)?;
                match op {
                    Some(op) => Ok(quote!(Ops::<STRIDE>::new(ip, Opcode::#op).#method::<#n>())),
                    None => Ok(quote!(Ops::<STRIDE>::from_ip(ip).#method::<#n>())),
                }
            }

            "next" | "dispatch" | "tail" | "reenter" | "dispatch_frame" => Err(syn::Error::new(
                method.span(),
                format!("`{method}` is a control transfer: write `{method}!(..)` instead"),
            )),

            _ => Err(syn::Error::new(
                method.span(),
                format!(
                    "unknown `Frame` method `{method}`: use `frame.ctx().{method}(..)` for context access"
                ),
            )),
        }
    }
}

/// Match `recv . method [::< .. >] (args)` starting at `i`. The
/// receiver must not be a field access (`x.recv`). Returns the method
/// name, an optional turbofish, the arguments, the span of the
/// argument group (so the emitted call can map back onto the written
/// one), and the next index.
fn match_method(
    tts: &[TokenTree],
    i: usize,
    recv: &str,
) -> Option<(Ident, Option<TokenStream2>, TokenStream2, Span, usize)> {
    let TokenTree::Ident(rcv) = &tts[i] else {
        return None;
    };
    if rcv != recv {
        return None;
    }
    if is_punct(tts.get(i.wrapping_sub(1)), '.') {
        return None;
    }
    let TokenTree::Punct(dot) = tts.get(i + 1)? else {
        return None;
    };
    if dot.as_char() != '.' {
        return None;
    }
    let TokenTree::Ident(method) = &tts.get(i + 2)? else {
        return None;
    };
    let method = method.clone();
    let mut j = i + 3;

    let mut turbo = None;
    if is_punct(tts.get(j), ':') && is_punct(tts.get(j + 1), ':') && is_punct(tts.get(j + 2), '<') {
        let mut depth = 1usize;
        j += 3;
        let start = j;
        loop {
            match tts.get(j)? {
                TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
                TokenTree::Punct(p) if p.as_char() == '>' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        turbo = Some(tts[start..j].iter().cloned().collect());
        j += 1;
    }

    let TokenTree::Group(g) = tts.get(j)? else {
        return None;
    };
    if g.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    Some((method, turbo, g.stream(), g.span(), j + 1))
}

fn is_punct(tt: Option<&TokenTree>, c: char) -> bool {
    matches!(tt, Some(TokenTree::Punct(p)) if p.as_char() == c)
}

/// Match `exit ! (args)` for a known control-transfer name (or a
/// removed one, so it can error well). The name must not be part of a
/// path (`::exit!`) or field access (`.exit!`). Returns the name, the
/// spans of the `!` and the argument group (so the emitted call can
/// map back onto the written one), the arguments, and the next index.
fn match_exit(tts: &[TokenTree], i: usize) -> Option<(Ident, Span, TokenStream2, Span, usize)> {
    let TokenTree::Ident(name) = &tts[i] else {
        return None;
    };
    if !EXITS.contains(&name.to_string().as_str()) {
        return None;
    }
    if is_punct(tts.get(i.wrapping_sub(1)), '.') || is_punct(tts.get(i.wrapping_sub(1)), ':') {
        return None;
    }
    let TokenTree::Punct(bang) = tts.get(i + 1)? else {
        return None;
    };
    if bang.as_char() != '!' {
        return None;
    }
    let TokenTree::Group(g) = tts.get(i + 2)? else {
        return None;
    };
    if g.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    Some((name.clone(), bang.span(), g.stream(), g.span(), i + 3))
}

/// Move the `!` and the argument group of the emitted macro call onto
/// the spans of the written ones: together with the interpolated name
/// ident this makes the whole `exit!(..)` map onto its source range,
/// so IDEs highlight and hover it as one resolved macro call. Tokens
/// that are not a macro call (plain identifiers, method chains) pass
/// through untouched.
fn respan_exit(ts: TokenStream2, bang: Span, group: Span) -> TokenStream2 {
    let mut out: Vec<TokenTree> = Vec::new();
    for (i, tt) in ts.into_iter().enumerate() {
        match (i, tt) {
            (1, TokenTree::Punct(p)) if p.as_char() == '!' => {
                let mut p = Punct::new('!', Spacing::Alone);
                p.set_span(bang);
                out.push(TokenTree::Punct(p));
            }
            (2, TokenTree::Group(g)) => {
                let mut g = Group::new(g.delimiter(), g.stream());
                g.set_span(group);
                out.push(TokenTree::Group(g));
            }
            (_, tt) => out.push(tt),
        }
    }
    out.into_iter().collect()
}

fn no_args(name: &Ident, args: &TokenStream2) -> syn::Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(syn::Error::new(
            name.span(),
            format!("`{name}` takes no arguments"),
        ))
    }
}

fn one_arg(name: &Ident, args: TokenStream2) -> syn::Result<TokenStream2> {
    match split_args(args).as_slice() {
        [a] if !a.is_empty() => Ok(a.clone()),
        _ => Err(syn::Error::new(
            name.span(),
            format!("`{name}(..)` takes exactly one argument"),
        )),
    }
}

fn split_args(args: TokenStream2) -> Vec<TokenStream2> {
    let mut out = vec![];
    let mut cur = vec![];
    let mut depth = 0usize;
    for tt in args {
        match &tt {
            TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
            TokenTree::Punct(p) if p.as_char() == '>' => depth -= 1,
            TokenTree::Punct(p) if p.as_char() == ',' && depth == 0 => {
                out.push(cur.into_iter().collect());
                cur = vec![];
                continue;
            }
            _ => {}
        }
        cur.push(tt);
    }
    if !cur.is_empty() {
        out.push(cur.into_iter().collect());
    }
    out
}

/// `slow!` instantiates its path at the handler's stride; a
/// hand-written turbofish would fight that.
fn reject_turbofish(name: &Ident, path: &TokenStream2) -> syn::Result<()> {
    let tts: Vec<TokenTree> = path.clone().into_iter().collect();
    if tts.windows(3).any(|w| {
        is_punct(Some(&w[0]), ':') && is_punct(Some(&w[1]), ':') && is_punct(Some(&w[2]), '<')
    }) {
        return Err(syn::Error::new(
            name.span(),
            "`tail!` appends `::<STRIDE>` itself; drop the turbofish",
        ));
    }
    Ok(())
}

/// Operand indices are const generics, so the turbofish must be a
/// single integer literal.
fn const_arg(method: &Ident, turbo: Option<TokenStream2>) -> syn::Result<TokenTree> {
    let tts: Vec<TokenTree> = turbo.unwrap_or_default().into_iter().collect();
    match tts.as_slice() {
        [tt @ TokenTree::Literal(_)] => Ok(tt.clone()),
        _ => Err(syn::Error::new(
            method.span(),
            format!("`frame.{method}::<I>()` requires a const integer index"),
        )),
    }
}
