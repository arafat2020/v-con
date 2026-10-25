use crate::domain::host:: EffectiveHost;
use std::collections::HashSet;

#[derive(Debug, PartialEq, Eq)]
pub enum Term{
    Exact(String),
    Globe(glob::Pattern),
    Tag(String),
    TagIntersection(Vec<String>),
    All,
    Exclude(Box<Term>),
}


pub fn parse_selector(input: &str) -> Result<Vec<Term>, String>{
    let mut terms: Vec<_> = Vec::new();
    for part in input.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()){
        if let Some(negated) = part.strip_prefix('!'){
            let inner_term = parse_single_term(negated)?;
            terms.push(Term::Exclude(Box::new(inner_term)));
        } else {
            terms.push(parse_single_term(part)?);
        }
    }

    Ok(terms)
}

fn parse_single_term(term_str: &str) -> Result<Term, String>{
    if term_str == "all"{
        Ok(Term::All)
    } else if term_str.starts_with('@') {
        let tag_str = &term_str[1..];
        if tag_str.contains('+') {
            let tags: Vec<String> = tag_str.split('+').map(|s| s.to_string()).collect();
            Ok(Term::TagIntersection(tags))
        } else {
            Ok(Term::Tag(tag_str.to_string()))
        }
    } else if term_str.contains('*') || term_str.contains('?') {
        glob::Pattern::new(term_str)
        .map(Term::Globe)
        .map_err(|e| format!("Invalid glob pattern '{}': {}", term_str, e))
    } else {
        Ok(Term::Exact(term_str.to_string()))
    }
}

pub fn evaluate_selector<'a>(
    terms: Vec<Term>,
    hosts: &'a [EffectiveHost],
) -> Vec<&'a EffectiveHost>{
    let has_only_exclude: bool = terms.iter().all(|t| matches!(t, Term::Exclude(_)));
    let mut matched_names: HashSet<&String> = HashSet::new();

    if has_only_exclude {
        for host in hosts.iter().filter(|h| !h.disabled) {
            matched_names.insert(&host.name);
        }
    } else {
        for term in &terms {
            for host in hosts.iter().filter(|h| !h.disabled){
               if matches_term_positive(term , host){
                matched_names.insert(&host.name);
               }
            }
        }
    }

    for term in terms {
        if let Term::Exclude(inner) = term {
            matched_names.retain(|name| {
                if let Some(host) = hosts.iter().find(|h| &h.name == *name){
                    !matches_term_positive(&inner, host)
                } else {
                    false
                }
            });
        }
    }

    hosts
    .iter()
    .filter(|h| {
        matched_names.contains(&h.name)
    })
    .collect()

}

fn matches_term_positive(term: &Term, host: &EffectiveHost) -> bool{
    match term {
        Term::Exact(name) => &host.name == name,
        Term::Globe(pattern) => pattern.matches(&host.name),
        Term::Tag(tag) => host.tags.contains(tag),
        Term::TagIntersection(tags) => tags.iter().all(|t| host.tags.contains(t)),
        Term::All => true,
        Term::Exclude(_) => false
    }
}