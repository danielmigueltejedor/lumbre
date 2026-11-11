//! How long a routing decision takes. Run with:
//! `cargo run --release -p lumbre-router --example route_bench`

use std::hint::black_box;
use std::time::Instant;

use lumbre_router::{Route, Router, Stage};

const REQUESTS: &[&str] = &[
    "¿Qué hora es?",
    "what's the date today",
    "¿Cuánto es 12 * (3 + 4)?",
    "busca el archivo presupuesto 2026",
    "hola",
    "Recomiéndame tres novelas cortas para leer este verano en la playa",
    "Explica paso a paso por qué la entropía de un sistema aislado no disminuye",
    "Fix this bug: fn main() { let x: u8 = 300; } does not compile",
    "why is my code slow",
    "Write a haiku about autumn rain and explain the imagery you chose in two sentences",
];

fn main() {
    let router = Router::new();
    let rounds = 200_000;
    let started = Instant::now();
    for _ in 0..rounds {
        for request in REQUESTS {
            black_box(router.route(black_box(request)));
        }
    }
    let per_request = started.elapsed().as_nanos() as f64 / f64::from(rounds) / REQUESTS.len() as f64;

    let decisions: Vec<_> = REQUESTS.iter().map(|request| router.route(request)).collect();
    let tools = decisions.iter().filter(|decision| matches!(decision.route, Route::Tool { .. })).count();
    let fallbacks = decisions.iter().filter(|decision| decision.stage == Stage::Fallback).count();
    println!("{} requests x {rounds} rounds: {per_request:.0} ns per routing decision", REQUESTS.len());
    println!(
        "of the {} sample requests, {tools} go to a tool (no model) and {fallbacks} are ambiguous",
        REQUESTS.len()
    );
}
