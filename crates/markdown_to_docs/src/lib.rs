use proc_macro::TokenStream;
use quote::quote;
use regex::Regex;

#[proc_macro]
pub fn include(input: TokenStream) -> TokenStream {
    let input_str = input.to_string();

    let ghf_markdown = std::fs::read_to_string(&input_str.trim_matches('"')).unwrap();

    let re = Regex::new(r"> \[!(CAUTION|IMPORTANT|NOTE|TIP|WARNING)\]").unwrap();
    let transformed_content = re
        .replace_all(&ghf_markdown, |caps: &regex::Captures| match &caps[1] {
            "CAUTION" => "> ☢️",
            "IMPORTANT" => "> 🚨",
            "NOTE" => "> 📝",
            "TIP" => "> 💡",
            "WARNING" => "> ⚠️",
            _ => unreachable!(),
        })
        .to_string();

    quote! {
      #transformed_content
    }
    .into()
}
