use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{parse_quote, spanned::Spanned, Error, Result};

use crate::{sig, utils};

pub fn process_trait(mode: utils::Mode, mut trait_: syn::ItemTrait) -> TokenStream {
    // The diagnostics and compile errors that we gather.
    let mut report = quote!();
    // trait ... { %trait_body }
    let mut trait_body = Vec::<syn::TraitItem>::new();
    // impl ... { %blank_body }
    let mut blank_body = Vec::<syn::ImplItem>::new();

    let name = trait_.ident.clone();
    let names = utils::collect_generics_names(&trait_.generics);

    // The fist generic is meant to be the name for the collection.
    let base_name = names
        .first()
        .cloned()
        .unwrap_or_else(|| syn::Ident::new("_", Span::call_site()));

    if names.is_empty() && mode == utils::Mode::WithCollection {
        report.extend(
            Error::new(trait_.generics.span(), "Missing collection generic.").to_compile_error(),
        );
    }

    let mut has_init = false;
    let mut has_post = false;

    for item in std::mem::take(&mut trait_.items) {
        match item {
            syn::TraitItem::Const(mut item) => match utils::impl_trait_const(&item) {
                Ok(impl_) => {
                    item.default = None;
                    trait_body.push(syn::TraitItem::Const(item));
                    blank_body.push(syn::ImplItem::Const(impl_));
                },
                Err(err) => {
                    report.extend(err.to_compile_error());
                },
            },

            syn::TraitItem::Fn(item) if mode == utils::Mode::BlankOnly => {
                if item.default.is_none() {
                    let impl_ = utils::impl_trait_fn(&item, default_blank_block());
                    blank_body.push(syn::ImplItem::Fn(impl_));
                }
                trait_body.push(syn::TraitItem::Fn(item));
            },

            syn::TraitItem::Fn(item) => {
                let name = item.sig.ident.to_string();

                match name.as_str() {
                    "_init" => match impl_init(&base_name, item) {
                        Ok(items) => {
                            has_init = true;
                            trait_body.extend(items.into_iter());
                        },
                        Err(err) => report.extend(err.to_compile_error()),
                    },
                    "_post" => match impl_post(&base_name, item) {
                        Ok(item) => {
                            has_post = true;
                            trait_body.push(item);
                        },
                        Err(err) => report.extend(err.to_compile_error()),
                    },
                    _name if item.default.is_some() => {
                        // method has default implementation. no need to include it in blank.
                        trait_body.push(syn::TraitItem::Fn(item));
                    },
                    _name => {
                        let impl_ = utils::impl_trait_fn(&item, default_blank_block());
                        trait_body.push(syn::TraitItem::Fn(item));
                        blank_body.push(syn::ImplItem::Fn(impl_));
                    },
                }
            },

            syn::TraitItem::Type(mut item) => {
                let impl_ = utils::impl_trait_type(&item);

                // Ignore the default for the trait type member since
                // it not supported in Rust.
                item.default = None;

                trait_body.push(syn::TraitItem::Type(item));
                blank_body.push(syn::ImplItem::Type(impl_));
            },

            // handle anything else.
            // TraitItem::Macro(_) | TraitItem::Verbatim(_) | _
            item => {
                // non-exhaustive enum member. If we don't know what the item is or if we
                // don't have anything to do with it just push it to the trait.
                trait_body.push(item);
            },
        }
    }

    if !has_init && mode == utils::Mode::WithCollection {
        trait_body.push(parse_quote! {
            #[doc(hidden)]
            fn infu_dependencies(visitor: &mut infusion::graph::DependencyGraphVisitor) {
                visitor.mark_input();
            }
        });

        trait_body.push(parse_quote! {
            #[doc(hidden)]
            fn infu_initialize(
                _container: &infusion::Container
            ) -> ::std::result::Result<
                Self,
                ::std::boxed::Box<dyn std::error::Error>
            > where Self: Sized {
                unreachable!("This trait is marked as an input.")
            }
        });
    }

    if !has_post && mode == utils::Mode::WithCollection {
        trait_body.push(parse_quote! {
            #[doc(hidden)]
            fn infu_post_initialize(&mut self, _container: &infusion::Container) {
                // empty.
            }
        });
    }

    // Set the trait items to what they are.
    trait_.items = trait_body;

    let blank = {
        let generics = &trait_.generics;

        let params = if names.is_empty() {
            quote!()
        } else {
            quote! { <#(#names),*> }
        };

        let blank = utils::generics_to_blank(generics);

        let maybe_init = if mode == utils::Mode::WithCollection {
            // overwrite the initialization.
            quote! {
                fn infu_initialize(
                    _container: &infusion::Container
                ) -> ::std::result::Result<
                    Self,
                    ::std::boxed::Box<dyn std::error::Error>
                > where Self: Sized {
                    Ok(Self::default())
                }
            }
        } else {
            quote! {}
        };

        quote! {
            impl #generics #name #params for #blank {
                #maybe_init

                #(#blank_body)*
            }
        }
    };

    quote! {
        #report

        #trait_

        #blank
    }
}

fn impl_init(base: &syn::Ident, item: syn::TraitItemFn) -> Result<Vec<syn::TraitItem>> {
    let Some(block) = &item.default else {
        return Err(Error::new(item.span(), "Infu init requires default implementation."));
    };

    let (deps, names) = sig::verify_fn_signature(sig::InfuFnKind::Init, &item.sig)?;
    let deps_tags = deps.iter().map(|d| utils::tag(base, d));
    let init_tags = deps.iter().map(|d| utils::tag(base, d));

    Ok(vec![
        parse_quote! {
            #[doc(hidden)]
            fn infu_dependencies(__visitor: &mut infusion::graph::DependencyGraphVisitor) {
            #(
                __visitor.add_dependency(
                    #deps_tags
                );
             )*
            }
        },
        parse_quote! {
            #[doc(hidden)]
            fn infu_initialize(
                __container: &infusion::Container
            ) -> ::std::result::Result<Self, ::std::boxed::Box<dyn std::error::Error>>
            where Self: Sized {
                #(
                    let #names: &<#base as Collection>::#deps = __container.get(#init_tags);
                 )*

                let __tmp: ::std::result::Result<Self, _> = {
                    #block
                };

                __tmp.map_err(|e| e.into())
            }
        },
    ])
}

fn impl_post(base: &syn::Ident, item: syn::TraitItemFn) -> Result<syn::TraitItem> {
    let Some(block) = &item.default else {
        return Err(Error::new(item.span(), "Infu post requires default implementation."));
    };

    let (deps, names) = sig::verify_fn_signature(sig::InfuFnKind::Post, &item.sig)?;
    let tags = deps.iter().map(|d| utils::tag(base, d));

    Ok(parse_quote! {
        #[doc(hidden)]
        fn infu_post_initialize(&mut self, __container: &infusion::Container) {
            #(
                let #names: &<#base as Collection>::#deps = __container.get(#tags);
             )*

            {
                #block
            };
        }
    })
}

/// The code block for a blank method implementation.
fn default_blank_block() -> syn::Block {
    parse_quote! {
        {
            panic!("BLANK METHOD CALLED");
        }
    }
}

