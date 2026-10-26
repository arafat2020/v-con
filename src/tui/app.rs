use crate::domain::host::EffectiveHost;
use crate::domain::selector::{evaluate_selector, parse_selector};


pub struct FilterResult<'a> {
    pub filter_hosts: Vec<&'a EffectiveHost>,
    pub selector_error: Option<String>
}

pub fn filter_hosts<'a>(hosts: &'a [EffectiveHost], query: &str)-> FilterResult<'a> {
    let trimmed = query.trim();

    if trimmed.is_empty(){
        return FilterResult{
            filter_hosts: hosts.iter().filter(|h|h.disabled).collect(),
            selector_error: None
        }
    }

    if trimmed.starts_with('@') || trimmed.starts_with('!') || trimmed.starts_with('*') {
        match parse_selector(trimmed) {
            Ok(terms) => FilterResult{
                filter_hosts: evaluate_selector(terms, hosts),
                selector_error: None
            },
            Err(err)=>FilterResult { 
                filter_hosts: Vec::new(), 
                selector_error: Some(err) 
            }
        }
    } else {
        let query_lower = trimmed.to_lowercase();
        let matched = hosts
        .iter()
        .filter(|h| {
            h.name.to_lowercase().contains(&query_lower)
            || h.address.to_lowercase().contains(&query_lower)
            || h.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            || h.note.as_deref().unwrap_or("").to_lowercase().contains(&query_lower) 
        }).collect();

        FilterResult { 
            filter_hosts: matched, 
            selector_error: None, 
        }
    }
    
}
