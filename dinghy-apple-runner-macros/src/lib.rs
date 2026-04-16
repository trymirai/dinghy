extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn test_case(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    let function_name = function.sig.ident.clone();

    quote! {
        #function

        ::inventory::submit! {
            ::dinghy_apple_runner_support::TestCase {
                name: ::core::concat!(::core::module_path!(), "::", ::core::stringify!(#function_name)),
                run: #function_name,
            }
        }
    }
    .into()
}

#[proc_macro_attribute]
pub fn bench_case(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    let function_name = function.sig.ident.clone();

    quote! {
        #function

        ::inventory::submit! {
            ::dinghy_apple_runner_support::BenchCase {
                name: ::core::concat!(::core::module_path!(), "::", ::core::stringify!(#function_name)),
                run: #function_name,
            }
        }
    }
    .into()
}

#[proc_macro_attribute]
pub fn ignored(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    let function_name = function.sig.ident.clone();

    quote! {
        #function

        ::inventory::submit! {
            ::dinghy_apple_runner_support::IgnoredCase {
                name: ::core::concat!(::core::module_path!(), "::", ::core::stringify!(#function_name)),
            }
        }
    }
    .into()
}
