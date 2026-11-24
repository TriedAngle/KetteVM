//! Dispatch-table generators for the interpreter backends.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Token, parse_macro_input, punctuated::Punctuated};

/// `tail_call_table!(Load => handler::op_load, ...)`
pub fn tail_call_table(input: TokenStream) -> TokenStream {
    let entries =
        parse_macro_input!(input with Punctuated::<TableEntry, Token![,]>::parse_terminated);
    let narrow = entries.iter().map(|entry| {
        let op = &entry.op;
        let handler = &entry.handler;
        quote!(t[Opcode::#op as usize] = #handler::<1> as Handler;)
    });
    let wide = entries.iter().map(|entry| {
        let op = &entry.op;
        let handler = &entry.handler;
        quote!(t[Opcode::#op as usize] = #handler::<2> as Handler;)
    });
    quote! {
        #[rustfmt::skip]
        const fn __tail_call_narrow() -> [Handler; 256] {
            let mut t = [op_trap::<1> as Handler; 256];
            #(#narrow)*
            t
        }

        #[rustfmt::skip]
        const fn __tail_call_wide() -> [Handler; 256] {
            let mut t = [op_trap::<2> as Handler; 256];
            #(#wide)*
            t
        }
    }
    .into()
}

/// `loop_call_table!(Load => handler::match_op_load, ...)`
pub fn loop_call_table(input: TokenStream) -> TokenStream {
    let entries =
        parse_macro_input!(input with Punctuated::<TableEntry, Token![,]>::parse_terminated);
    let narrow = entries.iter().map(|entry| {
        let op = &entry.op;
        let handler = &entry.handler;
        quote!(t[Opcode::#op as usize] = #handler::<1> as DebugHandler;)
    });
    let wide = entries.iter().map(|entry| {
        let op = &entry.op;
        let handler = &entry.handler;
        quote!(t[Opcode::#op as usize] = #handler::<2> as DebugHandler;)
    });
    quote! {
        #[rustfmt::skip]
        const fn __loop_call_narrow() -> [DebugHandler; 256] {
            let mut t = [debug_trap::<1> as DebugHandler; 256];
            #(#narrow)*
            t
        }

        #[rustfmt::skip]
        const fn __loop_call_wide() -> [DebugHandler; 256] {
            let mut t = [debug_trap::<2> as DebugHandler; 256];
            #(#wide)*
            t
        }

        static NARROW: [DebugHandler; 256] = __loop_call_narrow();
        static WIDE: [DebugHandler; 256] = __loop_call_wide();
    }
    .into()
}

struct TableEntry {
    op: syn::Ident,
    handler: syn::Path,
}

impl syn::parse::Parse for TableEntry {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let op = input.parse()?;
        input.parse::<Token![=>]>()?;
        let handler = input.parse()?;
        Ok(Self { op, handler })
    }
}

/// `match_step!(Load => handler::match_op_load::<STRIDE>, ...)`
pub fn match_step(input: TokenStream) -> TokenStream {
    let entries =
        parse_macro_input!(input with Punctuated::<StepEntry, Token![,]>::parse_terminated);
    let arms = entries.iter().map(|entry| {
        let op = &entry.op;
        match &entry.kind {
            StepKind::Plain(handler) => {
                quote!(Opcode::#op => #handler(*ip, *regs, acc, ctx, roots),)
            }
            StepKind::Regs(handler) => quote! {
                Opcode::#op => {
                    let next = #handler(*ip, *regs, acc, ctx, roots);
                    *regs = unsafe { Regs::new(ctx.regs_ptr()) };
                    *frame_base = ctx.frame_base();
                    next
                }
            },
            StepKind::Wide => quote! {
                Opcode::Wide => {
                    step::<2>(ctx, ip, regs, acc, frame_base, roots);
                    *ip
                }
            },
        }
    });
    quote! {
        *ip = match op {
            #(#arms)*
        };
    }
    .into()
}

struct StepEntry {
    op: syn::Ident,
    kind: StepKind,
}

enum StepKind {
    Plain(syn::Expr),
    Regs(syn::Expr),
    Wide,
}

impl syn::parse::Parse for StepEntry {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let op = input.parse()?;
        input.parse::<Token![=>]>()?;
        let kind = if input.peek(syn::Ident) {
            let fork = input.fork();
            let marker: syn::Ident = fork.parse()?;
            if marker == "regs" {
                input.parse::<syn::Ident>()?;
                StepKind::Regs(input.parse()?)
            } else if marker == "wide" {
                input.parse::<syn::Ident>()?;
                StepKind::Wide
            } else {
                StepKind::Plain(input.parse()?)
            }
        } else {
            StepKind::Plain(input.parse()?)
        };
        Ok(Self { op, kind })
    }
}
