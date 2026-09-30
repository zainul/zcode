use std::time::Duration;

use domain::SymbolKind;

use crate::extract::{parse, Extracted, ParseError};
use crate::lang::Lang;

fn run(lang: Lang, src: &str) -> Extracted {
    parse(lang, src, Duration::from_secs(5)).expect("parses")
}

fn quals(x: &Extracted) -> Vec<(String, SymbolKind)> {
    x.defs
        .iter()
        .map(|d| (d.qualified.clone(), d.kind))
        .collect()
}

const RUST: &str = r#"use std::collections::HashMap;
use crate::domain::{Tool, ToolResult};

/// A loop.
#[derive(Debug)]
pub struct AgentLoop<T> {
    turns: u32,
    inner: T,
}

impl<T: Clone> AgentLoop<T> {
    /// Runs it.
    pub fn execute(&self, n: u32) -> u32 {
        let helper = |x: u32| x + 1;
        helper(n)
    }
}

impl std::fmt::Display for AgentLoop<u8> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "loop")
    }
}

pub trait Runner {
    fn run(&self);
}

mod inner {
    pub const LIMIT: usize = 3;
    pub fn nested() {}
}

pub enum Mode { A, B }
type Alias = HashMap<String, u32>;
"#;

#[test]
fn rust_definitions_are_qualified_by_their_containers() {
    let x = run(Lang::Rust, RUST);
    let q = quals(&x);
    for want in [
        ("AgentLoop", SymbolKind::Struct),
        ("AgentLoop", SymbolKind::Impl),
        ("AgentLoop::execute", SymbolKind::Method),
        ("AgentLoop::fmt", SymbolKind::Method),
        ("Runner", SymbolKind::Trait),
        ("Runner::run", SymbolKind::Method),
        ("inner", SymbolKind::Module),
        ("inner::LIMIT", SymbolKind::Const),
        ("inner::nested", SymbolKind::Function),
        ("Mode", SymbolKind::Enum),
        ("Alias", SymbolKind::Type),
    ] {
        assert!(
            q.contains(&(want.0.to_string(), want.1)),
            "missing {want:?} in {q:?}"
        );
    }
    assert_eq!(x.error_nodes, 0);
    assert_eq!(
        x.imports,
        [
            "std::collections::HashMap",
            "crate::domain::{Tool, ToolResult}"
        ]
    );
}

#[test]
fn spans_take_in_doc_comments_and_attributes_and_bodies_are_separate() {
    let x = run(Lang::Rust, RUST);
    let s = x
        .defs
        .iter()
        .find(|d| d.kind == SymbolKind::Struct)
        .expect("struct");
    assert_eq!((s.span.start_line, s.span.end_line), (4, 9));
    assert_eq!(s.name_line, 6);
    assert_eq!(s.name_col, 12);
    assert_eq!(s.signature, "pub struct AgentLoop<T> {");
    let m = x.defs.iter().find(|d| d.name == "execute").expect("method");
    assert_eq!((m.span.start_line, m.span.end_line), (12, 16));
    let body = m.body.expect("body");
    assert_eq!((body.start_line, body.end_line), (13, 16));
    assert_eq!(
        &RUST[body.start_byte as usize..body.start_byte as usize + 1],
        "{"
    );
    assert_eq!(m.depth, 1);
}

#[test]
fn identifiers_are_recorded_once_per_line() {
    let x = run(Lang::Rust, RUST);
    let helper: Vec<u32> = x
        .idents
        .iter()
        .filter(|(n, _)| n == "helper")
        .map(|(_, l)| *l)
        .collect();
    assert_eq!(helper, [14, 15]);
}

#[test]
fn syntax_errors_are_counted_with_the_first_position() {
    let x = run(Lang::Rust, "fn ok() {}\nfn broken( {\n");
    assert!(x.error_nodes > 0);
    assert_eq!(x.first_error.map(|(l, _)| l), Some(2));
}

#[test]
fn go_methods_are_qualified_by_receiver_and_typed_specs_by_their_type() {
    let src = r#"package api

import (
	"fmt"
	"net/http"
)

// Server serves.
type Server struct {
	addr string
}

type Store interface {
	Get(id string) error
}

type ID string

const Limit = 10

func New() *Server { return &Server{} }

func (s *Server) Run(w http.ResponseWriter) {
	fmt.Println(s.addr)
}
"#;
    let x = run(Lang::Go, src);
    let q = quals(&x);
    assert_eq!(
        q,
        [
            ("Server".to_string(), SymbolKind::Struct),
            ("Store".to_string(), SymbolKind::Interface),
            ("ID".to_string(), SymbolKind::Type),
            ("Limit".to_string(), SymbolKind::Const),
            ("New".to_string(), SymbolKind::Function),
            ("Server.Run".to_string(), SymbolKind::Method),
        ]
    );
    let server = &x.defs[0];
    assert_eq!(
        server.span.start_line, 8,
        "doc comment and `type` keyword included"
    );
    assert_eq!(x.imports, ["fmt", "net/http"]);
}

#[test]
fn typescript_exports_classes_and_module_level_arrows() {
    let src = r#"import { z } from "zod";
import React from 'react';

export interface User { id: string }
export type Id = string;
export enum Color { Red }

/** Service. */
export class UserService {
  create(u: User): User {
    const local = 1;
    return u;
  }
}

export const handler = async (req: Request) => {
  return req;
};

const LIMIT = 5;

function helper() {
  const inner = () => 1;
  return inner();
}
"#;
    let x = run(Lang::TypeScript, src);
    let q = quals(&x);
    assert_eq!(
        q,
        [
            ("User".to_string(), SymbolKind::Interface),
            ("Id".to_string(), SymbolKind::Type),
            ("Color".to_string(), SymbolKind::Enum),
            ("UserService".to_string(), SymbolKind::Class),
            ("UserService.create".to_string(), SymbolKind::Method),
            ("handler".to_string(), SymbolKind::Function),
            ("LIMIT".to_string(), SymbolKind::Const),
            ("helper".to_string(), SymbolKind::Function),
        ]
    );
    let class = &x.defs[3];
    assert_eq!(class.span.start_line, 8, "doc comment and export included");
    let handler = &x.defs[5];
    assert_eq!(handler.span.start_line, 16);
    assert!(handler.body.is_some(), "arrow body is addressable");
    assert_eq!(x.imports, ["zod", "react"]);
}

#[test]
fn tsx_parses_jsx() {
    let src = "export function Page() {\n  return <div className=\"x\">hi</div>;\n}\n";
    let x = run(Lang::Tsx, src);
    assert_eq!(quals(&x), [("Page".to_string(), SymbolKind::Function)]);
    assert_eq!(x.error_nodes, 0);
}

#[test]
fn python_methods_decorators_and_imports() {
    let src = r#"import os
from pkg.models import User

@dataclass
class Repo:
    """Repo."""

    @staticmethod
    def load(path):
        def inner():
            return 1
        return inner()

def main():
    pass
"#;
    let x = run(Lang::Python, src);
    assert_eq!(
        quals(&x),
        [
            ("Repo".to_string(), SymbolKind::Class),
            ("Repo.load".to_string(), SymbolKind::Method),
            ("Repo.load.inner".to_string(), SymbolKind::Function),
            ("main".to_string(), SymbolKind::Function),
        ]
    );
    assert_eq!(x.defs[0].span.start_line, 4, "decorator included");
    assert_eq!(x.defs[1].span.start_line, 8);
    assert_eq!(x.imports, ["os", "pkg.models"]);
}

#[test]
fn columns_count_characters_not_bytes() {
    let x = run(Lang::Rust, "/* é */ fn f() {}\n");
    assert_eq!(x.defs[0].name_col, 12);
}

#[test]
fn a_parse_over_budget_is_a_timeout() {
    let big = (0..20_000)
        .map(|i| format!("fn f{i}() {{ let x = {i}; }}\n"))
        .collect::<Vec<_>>()
        .concat();
    assert_eq!(
        parse(Lang::Rust, &big, Duration::ZERO),
        Err(ParseError::Timeout)
    );
}
