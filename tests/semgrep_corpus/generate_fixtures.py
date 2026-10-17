#!/usr/bin/env python3
"""Regenerate the provisional Semgrep cross-language corpus fixtures."""
from pathlib import Path

ROOT = Path(__file__).parent / "sources"
LANGUAGES = {
    "bash": ("sh", "bash"),
    "c": ("c", "c"),
    "cpp": ("cpp", "cpp"),
    "csharp": ("cs", "csharp"),
    "clojure": ("clj", "clojure"),
    "dart": ("dart", "dart"),
    "go": ("go", "go"),
    "hack": ("hack", "hack"),
    "java": ("java", "java"),
    "javascript": ("js", "javascript"),
    "julia": ("jl", "julia"),
    "kotlin": ("kt", "kotlin"),
    "lua": ("lua", "lua"),
    "ocaml": ("ml", "ocaml"),
    "php": ("php", "php"),
    "python": ("py", "python"),
    "r": ("r", "r"),
    "ruby": ("rb", "ruby"),
    "rust": ("rs", "rust"),
    "scala": ("scala", "scala"),
    "scheme": ("scm", "scheme"),
    "solidity": ("sol", "solidity"),
    "swift": ("swift", "swift"),
    "typescript": ("ts", "typescript"),
    "move": ("move", "move"),
}
SEED_COUNTS = {
    "dart": 180,
    "julia": 180,
    "clojure": 100,
    "ocaml": 105,
    "scheme": 100,
    "solidity": 63,
    "move": 63,
    "go": 62,
    "scala": 61,
    "rust": 61,
    "bash": 61,
    "javascript": 61,
    "kotlin": 61,
    "typescript": 61,
}


def marker(language: str, value: str) -> str:
    if language in {"bash", "julia", "python", "r", "ruby"}:
        return f"# {value}"
    if language == "clojure":
        return f";; {value}"
    if language == "lua":
        return f"-- {value}"
    if language == "ocaml":
        return f"(* {value} *)"
    if language == "scheme":
        return f"; {value}"
    return f"// {value}"


def render(language: str, slot: str) -> str:
    entity = (
        f"repodb_{language}_{slot}_entity_v1"
        if language == "ocaml"
        else f"Repodb{language.title()}{slot.title()}EntityV1"
    )
    lines: list[str] = [marker(language, "REPODB_SYNTAX_BEGIN")]

    if language in {"java", "csharp", "kotlin", "dart", "swift", "scala", "javascript", "typescript"}:
        if language == "java":
            lines += ["import java.util.*;", f"class {entity} {{", "  final int id;", "  String name;", "  boolean active;", f"  {entity}(int id, String name) {{ this.id = id; this.name = name; this.active = true; }}", "}", "class AuditEvent { final int id; final String action; AuditEvent(int id, String action) { this.id = id; this.action = action; } }", "class Registry {", "  private final Map<Integer, Object> entities = new HashMap<>();", "  private final List<AuditEvent> events = new ArrayList<>();", "  void add(int id, Object value) { entities.put(id, value); events.add(new AuditEvent(id, \"create\")); }", "  Object find(int id) { return entities.get(id); }", "  void archive(int id) { entities.remove(id); events.add(new AuditEvent(id, \"archive\")); }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"class Seed{i:03} {{", f"  static {entity} create() {{ return new {entity}({i}, \"record-{i:03}\"); }}", "}"]
            lines += ["class Program {", "  static void run() {", "    Registry registry = new Registry();", "    registry.add(1, Seed001.create());", "    registry.find(1);", "    registry.archive(1);", "  }", "}"]
        elif language == "csharp":
            lines += ["using System;", "using System.Collections.Generic;", f"record {entity}(int Id, string Name, bool Active = true);", "record AuditEvent(int Id, string Action);", "class Registry {", "  readonly Dictionary<int, object> entities = new();", "  readonly List<AuditEvent> events = new();", "  public void Add(int id, object value) { entities[id] = value; events.Add(new(id, \"create\")); }", "  public object? Find(int id) => entities.GetValueOrDefault(id);", "  public void Archive(int id) { entities.Remove(id); events.Add(new(id, \"archive\")); }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"static class Seed{i:03} {{", f"  public static {entity} Create() => new({i}, \"record-{i:03}\");", "}"]
            lines += ["class Program {", "  static void Main() {", "    var registry = new Registry();", "    registry.Add(1, Seed001.Create());", "    registry.Find(1);", "    registry.Archive(1);", "  }", "}"]
        elif language in {"javascript", "typescript"}:
            ts_fields = "  id: number; name: string; active: boolean;" if language == "typescript" else ""
            ts_ctor = "constructor(public id: number, public name: string, public active = true) {}" if language == "typescript" else "constructor(id, name) { this.id = id; this.name = name; this.active = true; }"
            result_type = f": {entity}" if language == "typescript" else ""
            lines += [f"class {entity} {{", ts_fields, ts_ctor, "}", "class AuditEvent { constructor(id, action) { this.id = id; this.action = action; } }", "class Registry {", "  constructor() { this.entities = new Map(); this.events = []; }", "  add(id, value) { this.entities.set(id, value); this.events.push(new AuditEvent(id, 'create')); }", "  find(id) { return this.entities.get(id); }", "  archive(id) { this.entities.delete(id); this.events.push(new AuditEvent(id, 'archive')); }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"function seed{i:03}(){result_type} {{", f"  return new {entity}({i}, 'record-{i:03}');", "}"]
            lines += ["function run() {", "  const registry = new Registry();", "  registry.add(1, seed001());", "  registry.find(1);", "  registry.archive(1);", "}"]
        elif language == "kotlin":
            lines += [f"data class {entity}(val id: Int, var name: String, var active: Boolean = true)", "data class AuditEvent(val id: Int, val action: String)", "class Registry {", "  val entities = mutableMapOf<Int, Any>()", "  val events = mutableListOf<AuditEvent>()", "  fun add(id: Int, value: Any) { entities[id] = value; events.add(AuditEvent(id, \"create\")) }", "  fun find(id: Int): Any? = entities[id]", "  fun archive(id: Int) { entities.remove(id); events.add(AuditEvent(id, \"archive\")) }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"object Seed{i:03} {{", f"  fun create(): {entity} = {entity}({i}, \"record-{i:03}\")", "}"]
            lines += ["fun runRegistry() {", "  val registry = Registry()", "  registry.add(1, Seed001.create())", "  registry.find(1)", "  registry.archive(1)", "}"]
        elif language == "dart":
            lines += [f"class {entity} {{", "  final int id;", "  String name;", "  bool active;", f"  {entity}(this.id, this.name, {{this.active = true}});", "}", "class AuditEvent { final int id; final String action; AuditEvent(this.id, this.action); }", "class Registry {", "  final Map<int, Object> entities = {};", "  final List<AuditEvent> events = [];", "  void add(int id, Object value) { entities[id] = value; events.add(AuditEvent(id, 'create')); }", "  Object? find(int id) => entities[id];", "  void archive(int id) { entities.remove(id); events.add(AuditEvent(id, 'archive')); }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"{entity} seed{i:03}() => {entity}({i}, 'record-{i:03}');"]
            lines += ["void runRegistry() {", "  final registry = Registry();", "  registry.add(1, seed001());", "  registry.find(1);", "  registry.archive(1);", "}"]
        elif language == "swift":
            lines += [f"struct {entity} {{", "  let id: Int", "  var name: String", "  var active: Bool = true", "}", "struct AuditEvent { let id: Int; let action: String }", "final class Registry {", "  private var entities: [Int: Any] = [:]", "  private(set) var events: [AuditEvent] = []", "  func add(_ id: Int, _ value: Any) { entities[id] = value; events.append(AuditEvent(id: id, action: \"create\")) }", "  func find(_ id: Int) -> Any? { entities[id] }", "  func archive(_ id: Int) { entities.removeValue(forKey: id); events.append(AuditEvent(id: id, action: \"archive\")) }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"func seed{i:03}() -> {entity} {{", f"  {entity}(id: {i}, name: \"record-{i:03}\")", "}"]
            lines += ["func runRegistry() {", "  let registry = Registry()", "  registry.add(1, seed001())", "  _ = registry.find(1)", "  registry.archive(1)", "}"]
        else:  # Scala
            lines += [f"case class {entity}(id: Int, name: String, active: Boolean = true)", "case class AuditEvent(id: Int, action: String)", "class Registry {", "  private val entities = scala.collection.mutable.Map.empty[Int, Any]", "  val events = scala.collection.mutable.ArrayBuffer.empty[AuditEvent]", "  def add(id: Int, value: Any): Unit = { entities(id) = value; events += AuditEvent(id, \"create\") }", "  def find(id: Int): Option[Any] = entities.get(id)", "  def archive(id: Int): Unit = { entities.remove(id); events += AuditEvent(id, \"archive\") }", "}"]
            for i in range(1, SEED_COUNTS.get(language, 60) + 1):
                lines += [f"object Seed{i:03} {{", f"  def create(): {entity} = {entity}({i}, \"record-{i:03}\")", "}"]
            lines += ["object Program {", "  def run(): Unit = {", "    val registry = new Registry", "    registry.add(1, Seed001.create())", "    registry.find(1)", "    registry.archive(1)", "  }", "}"]

    elif language in {"c", "cpp"}:
        lines += ["#include <stdbool.h>", "#include <stddef.h>", "#include <stdio.h>", f"typedef struct {{ int id; const char *name; bool active; }} {entity};", "typedef struct { int entity_id; const char *action; } AuditEvent;", f"typedef struct {{ int count; {entity} entities[64]; int event_count; AuditEvent events[128]; }} Registry;", "static void registry_log(Registry *registry, int id, const char *action) {", "  if (registry->event_count < 128) { registry->events[registry->event_count++] = (AuditEvent){id, action}; }", "}", f"static void registry_add(Registry *registry, {entity} value) {{", "  if (registry->count < 64) { registry->entities[registry->count++] = value; }", "  registry_log(registry, value.id, \"create\");", "}", f"static {entity} *registry_find(Registry *registry, int id) {{", "  for (int i = 0; i < registry->count; ++i) { if (registry->entities[i].id == id) return &registry->entities[i]; }", "  return NULL;", "}", f"static void registry_archive(Registry *registry, int id) {{", f"  {entity} *value = registry_find(registry, id);", "  if (value != NULL) { value->active = false; }", "  registry_log(registry, id, \"archive\");", "}"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"static {entity} seed_{i:03}(void) {{", f"  return ({entity}){{{i}, \"record-{i:03}\", true}};", "}"]
        lines += ["int main(void) {", "  Registry registry = {0};", "  registry_add(&registry, seed_001());", "  (void)registry_find(&registry, 1);", "  registry_archive(&registry, 1);", "  return 0;", "}"]

    elif language == "go":
        lines += ["package main", f"type {entity} struct {{ ID int; Name string; Active bool }}", "type AuditEvent struct { EntityID int; Action string }", "type Registry struct { Entities map[int]any; Events []AuditEvent }", "func (r *Registry) Add(id int, value any) { r.Entities[id] = value; r.Events = append(r.Events, AuditEvent{id, \"create\"}) }", "func (r *Registry) Find(id int) any { return r.Entities[id] }", "func (r *Registry) Archive(id int) { delete(r.Entities, id); r.Events = append(r.Events, AuditEvent{id, \"archive\"}) }"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"func seed{i:03}() {entity} {{", f"  return {entity}{{ID: {i}, Name: \"record-{i:03}\", Active: true}}", "}"]
        lines += ["func main() {", "  registry := Registry{Entities: map[int]any{}}", "  registry.Add(1, seed001())", "  _ = registry.Find(1)", "  registry.Archive(1)", "}"]

    elif language == "rust":
        lines += ["#[derive(Clone, Debug)]", f"struct {entity} {{ id: u64, name: String, active: bool }}", "#[derive(Clone, Debug)]", "struct AuditEvent { entity_id: u64, action: String }", "#[derive(Default)]", f"struct Registry {{ entities: Vec<{entity}>, events: Vec<AuditEvent> }}", "impl Registry {", f"  fn add(&mut self, value: {entity}) {{ self.events.push(AuditEvent {{ entity_id: value.id, action: String::from(\"create\") }}); self.entities.push(value); }}", f"  fn find(&self, id: u64) -> Option<&{entity}> {{ self.entities.iter().find(|value| value.id == id) }}", "  fn archive(&mut self, id: u64) { if let Some(value) = self.entities.iter_mut().find(|value| value.id == id) { value.active = false; } self.events.push(AuditEvent { entity_id: id, action: String::from(\"archive\") }); }", "}"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"fn seed_{i:03}() -> {entity} {{", f"  {entity} {{ id: {i}, name: String::from(\"record-{i:03}\"), active: true }}", "}"]
        lines += ["fn main() {", "  let mut registry = Registry::default();", "  registry.add(seed_001());", "  let _ = registry.find(1);", "  registry.archive(1);", "}"]

    elif language == "python":
        lines += ["from dataclasses import dataclass", "from typing import Dict, List, Optional", "", "@dataclass", f"class {entity}:", "    entity_id: int", "    name: str", "    active: bool = True", "", "@dataclass", "class AuditEvent:", "    entity_id: int", "    action: str", "", "class Registry:", "    def __init__(self) -> None:", "        self.entities: Dict[int, object] = {}", "        self.events: List[AuditEvent] = []", "    def add(self, entity_id: int, value: object) -> None:", "        self.entities[entity_id] = value", "        self.events.append(AuditEvent(entity_id, 'create'))", "    def find(self, entity_id: int) -> Optional[object]:", "        return self.entities.get(entity_id)", "    def archive(self, entity_id: int) -> None:", "        self.entities.pop(entity_id, None)", "        self.events.append(AuditEvent(entity_id, 'archive'))", ""]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"def seed_{i:03}() -> {entity}:", f"    return {entity}({i}, 'record-{i:03}')", ""]
        lines += ["def run_registry() -> None:", "    registry = Registry()", "    registry.add(1, seed_001())", "    registry.find(1)", "    registry.archive(1)", ""]

    elif language == "ruby":
        lines += [f"class {entity}", "  attr_accessor :id, :name, :active", "  def initialize(id, name)", "    @id, @name, @active = id, name, true", "  end", "end", "class AuditEvent", "  attr_reader :entity_id, :action", "  def initialize(entity_id, action); @entity_id, @action = entity_id, action; end", "end", "class Registry", "  def initialize; @entities, @events = {}, []; end", "  def add(id, value); @entities[id] = value; @events << AuditEvent.new(id, 'create'); end", "  def find(id); @entities[id]; end", "  def archive(id); @entities.delete(id); @events << AuditEvent.new(id, 'archive'); end", "end"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"def seed_{i:03}", f"  {entity}.new({i}, 'record-{i:03}')", "end"]
        lines += ["def run_registry", "  registry = Registry.new", "  registry.add(1, seed_001)", "  registry.find(1)", "  registry.archive(1)", "end"]

    elif language == "bash":
        lines += ["declare -A registry_entities=()", "declare -a registry_events=()", "registry_add() {", "  local key=\"$1\"; local value=\"$2\"", "  registry_entities[\"$key\"]=\"$value\"", "  registry_events+=(\"create:$key\")", "}", "registry_find() { printf '%s\\n' \"${registry_entities[$1]}\"; }", "registry_archive() { unset 'registry_entities[$1]'; registry_events+=(\"archive:$1\"); }"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"seed_{i:03}() {{", f"  printf '%s\\n' '{entity}:{i:03}:active'", "}"]
        lines += ["main() {", "  registry_add 1 \"$(seed_001)\"", "  registry_find 1", "  registry_archive 1", "}", "main \"$@\""]

    elif language == "clojure":
        lines += ["(ns repodb.registry)", f"(defrecord {entity} [id name active])", "(defrecord AuditEvent [entity-id action])", "(defrecord Registry [entities events])", "(defn empty-registry [] (->Registry {} []))", "(defn add-entity [registry entity]", "  (->Registry (assoc (:entities registry) (:id entity) entity)", "             (conj (:events registry) (->AuditEvent (:id entity) :create))))", "(defn find-entity [registry id] (get (:entities registry) id))", "(defn archive-entity [registry id]", "  (->Registry (dissoc (:entities registry) id)", "             (conj (:events registry) (->AuditEvent id :archive))))"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"(defn seed-{i:03} []", f"  (->{entity} {i} \"record-{i:03}\" true))"]
        lines += ["(defn run-registry []", "  (let [registry (empty-registry)", "        added (add-entity registry (seed-001))]", "    (find-entity added 1)", "    (archive-entity added 1)))"]

    elif language == "hack":
        lines[0:0] = ["<?hh"]
        lines += [f"final class {entity} {{", "  public function __construct(public int $id, public string $name, public bool $active = true) {}", "}", "final class AuditEvent {", "  public function __construct(public int $entityId, public string $action) {}", "}", "final class Registry {", "  public dict<int, mixed> $entities = dict[];", "  public vec<AuditEvent> $events = vec[];", "  public function add(int $id, mixed $value): void { $this->entities[$id] = $value; $this->events[] = new AuditEvent($id, 'create'); }", "  public function find(int $id): mixed { return $this->entities[$id] ?? null; }", "  public function archive(int $id): void { unset($this->entities[$id]); $this->events[] = new AuditEvent($id, 'archive'); }", "}"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"function seed_{i:03}(): {entity} {{", f"  return new {entity}({i}, 'record-{i:03}');", "}"]
        lines += ["function run_registry(): void {", "  $registry = new Registry();", "  $registry->add(1, seed_001());", "  $registry->find(1);", "  $registry->archive(1);", "}"]

    elif language == "julia":
        lines += [f"struct {entity}", "    id::Int", "    name::String", "    active::Bool", "end", "struct AuditEvent", "    entity_id::Int", "    action::String", "end", "mutable struct Registry", "    entities::Dict{Int,Any}", "    events::Vector{AuditEvent}", "end", "Registry() = Registry(Dict{Int,Any}(), AuditEvent[])", "function add!(registry::Registry, id::Int, value)", "    registry.entities[id] = value", "    push!(registry.events, AuditEvent(id, \"create\"))", "end", "find(registry::Registry, id::Int) = get(registry.entities, id, nothing)", "function archive!(registry::Registry, id::Int)", "    delete!(registry.entities, id)", "    push!(registry.events, AuditEvent(id, \"archive\"))", "end"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"seed_{i:03}() = {entity}({i}, \"record-{i:03}\", true)"]
        lines += ["function run_registry()", "    registry = Registry()", "    add!(registry, 1, seed_001())", "    find(registry, 1)", "    archive!(registry, 1)", "end"]

    elif language == "lua":
        lines += [f"local {entity} = {{}}", f"{entity}.__index = {entity}", f"function {entity}.new(id, name)", f"  return setmetatable({{ id = id, name = name, active = true }}, {entity})", "end", "local AuditEvent = {}", "AuditEvent.__index = AuditEvent", "local Registry = {}", "Registry.__index = Registry", "function Registry.new() return setmetatable({ entities = {}, events = {} }, Registry) end", "function Registry:add(id, value) self.entities[id] = value; table.insert(self.events, { id = id, action = 'create' }) end", "function Registry:find(id) return self.entities[id] end", "function Registry:archive(id) self.entities[id] = nil; table.insert(self.events, { id = id, action = 'archive' }) end"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"function seed_{i:03}()", f"  return {entity}.new({i}, 'record-{i:03}')", "end"]
        lines += ["local function run_registry()", "  local registry = Registry.new()", "  registry:add(1, seed_001())", "  registry:find(1)", "  registry:archive(1)", "end", "run_registry()"]

    elif language == "ocaml":
        lines += [f"type {entity} = {{ id : int; name : string; active : bool }}", f"type entity = {entity}", "type audit_event = { entity_id : int; action : string }",  "type registry = { mutable entities : entity list; mutable events : audit_event list }", "let empty_registry () = { entities = []; events = [] }", "let add registry entity =", "  registry.entities <- entity :: registry.entities;", "  registry.events <- { entity_id = entity.id; action = \"create\" } :: registry.events", "let find registry id = List.find_opt (fun entity -> entity.id = id) registry.entities", "let archive registry id =", "  registry.entities <- List.filter (fun entity -> entity.id <> id) registry.entities;", "  registry.events <- { entity_id = id; action = \"archive\" } :: registry.events"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"let seed_{i:03} () =", f"  ({{ id = {i}; name = \"record-{i:03}\"; active = true }} : entity)"]
        lines += ["let run_registry () =", "  let registry = empty_registry () in", "  add registry (seed_001 ());", "  ignore (find registry 1);", "  archive registry 1"]

    elif language == "php":
        lines[0:0] = ["<?php"]
        lines += [f"final class {entity} {{", "  public function __construct(public int $id, public string $name, public bool $active = true) {}", "}", "final class AuditEvent {", "  public function __construct(public int $entityId, public string $action) {}", "}", "final class Registry {", "  private array $entities = [];", "  private array $events = [];", "  public function add(int $id, object $value): void { $this->entities[$id] = $value; $this->events[] = new AuditEvent($id, 'create'); }", "  public function find(int $id): ?object { return $this->entities[$id] ?? null; }", "  public function archive(int $id): void { unset($this->entities[$id]); $this->events[] = new AuditEvent($id, 'archive'); }", "}"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"function seed_{i:03}(): {entity} {{", f"  return new {entity}({i}, 'record-{i:03}');", "}"]
        lines += ["function run_registry(): void {", "  $registry = new Registry();", "  $registry->add(1, seed_001());", "  $registry->find(1);", "  $registry->archive(1);", "}"]

    elif language == "r":
        lines += [f"{entity} <- function(id, name, active = TRUE) {{", "  structure(list(id = id, name = name, active = active), class = 'registry_entity')", "}", "AuditEvent <- function(entity_id, action) list(entity_id = entity_id, action = action)", "Registry <- function() {", "  state <- new.env(parent = emptyenv())", "  state$entities <- list()", "  state$events <- list()", "  state", "}", "registry_add <- function(registry, id, value) {", "  registry$entities[[as.character(id)]] <- value", "  registry$events[[length(registry$events) + 1]] <- AuditEvent(id, 'create')", "}", "registry_find <- function(registry, id) registry$entities[[as.character(id)]]", "registry_archive <- function(registry, id) {", "  registry$entities[[as.character(id)]] <- NULL", "  registry$events[[length(registry$events) + 1]] <- AuditEvent(id, 'archive')", "}"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"seed_{i:03} <- function() {{", f"  {entity}({i}, 'record-{i:03}')", "}"]
        lines += ["run_registry <- function() {", "  registry <- Registry()", "  registry_add(registry, 1, seed_001())", "  registry_find(registry, 1)", "  registry_archive(registry, 1)", "}"]

    elif language == "solidity":
        lines += ["pragma solidity ^0.8.20;", f"contract Registry{language.title()}{slot.title()} {{", f"  struct {entity} {{ uint256 id; string name; bool active; }}", "  struct AuditEvent { uint256 entityId; bytes32 action; }", f"  mapping(uint256 => {entity}) private entities;", "  AuditEvent[] private events;", "  event Changed(uint256 indexed id, bytes32 action);", f"  function add({entity} memory value) public {{ entities[value.id] = value; events.push(AuditEvent(value.id, keccak256(\"create\"))); emit Changed(value.id, keccak256(\"create\")); }}", f"  function find(uint256 id) public view returns ({entity} memory) {{ return entities[id]; }}", "  function archive(uint256 id) public { delete entities[id]; events.push(AuditEvent(id, keccak256(\"archive\"))); emit Changed(id, keccak256(\"archive\")); }"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"  function seed{i:03}() public pure returns ({entity} memory) {{", f"    return {entity}({i}, \"record-{i:03}\", true);", "  }"]
        lines += ["}"]

    elif language == "scheme":
        lines += [f"(define-record-type <{entity}>", f"  (make-{entity} id name active)", f"  {entity}?", "  (id entity-id)", "  (name entity-name)", "  (active entity-active))", "(define-record-type <audit-event>", "  (make-audit-event entity-id action)", "  audit-event?", "  (entity-id audit-entity-id)", "  (action audit-action))", "(define (make-registry) (vector '() '()))", "(define (registry-add registry value)", "  (vector-set! registry 0 (cons value (vector-ref registry 0)))", "  (vector-set! registry 1 (cons (make-audit-event (entity-id value) 'create) (vector-ref registry 1)))", "  registry)", "(define (registry-find registry id)", "  (find (lambda (value) (= (entity-id value) id)) (vector-ref registry 0)))", "(define (registry-archive registry id)", "  (vector-set! registry 0 (filter (lambda (value) (not (= (entity-id value) id))) (vector-ref registry 0)))", "  (vector-set! registry 1 (cons (make-audit-event id 'archive) (vector-ref registry 1))))"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"(define (seed-{i:03})", f"  (make-{entity} {i} \"record-{i:03}\" #t))"]
        lines += ["(define (run-registry)", "  (let ((registry (make-registry)))", "    (registry-add registry (seed-001))", "    (registry-find registry 1)", "    (registry-archive registry 1)))"]

    elif language == "move":
        lines += ["module 0x1::repodb_registry {", f"  struct {entity} has store, drop {{ id: u64, name: vector<u8>, active: bool }}", "  struct AuditEvent has store, drop { entity_id: u64, action: vector<u8> }", f"  struct Registry has key {{ entities: vector<{entity}>, events: vector<AuditEvent> }}", f"  public fun make_entity(id: u64): {entity} {{ {entity} {{ id, name: b\"record\", active: true }} }}", f"  public fun add_entity(registry: &mut Registry, entity: {entity}) {{ let id = entity.id; registry.entities.push_back(entity); registry.events.push_back(AuditEvent {{ entity_id: id, action: b\"create\" }}); }}", f"  public fun find_entity(registry: &Registry, id: u64): bool {{ registry.entities.contains(&make_entity(id)) }}", "  public fun archive_entity(registry: &mut Registry, id: u64) { let entity = vector::borrow_mut(&mut registry.entities, 0); entity.active = false; registry.events.push_back(AuditEvent { entity_id: id, action: b\"archive\" }); }"]
        for i in range(1, SEED_COUNTS.get(language, 60) + 1):
            lines += [f"  public fun seed_{i:03}(): {entity} {{", f"    make_entity({i})", "  }"]
        lines += ["}"]

    else:
        raise ValueError(f"No renderer for {language}")

    lines.append(marker(language, "REPODB_SYNTAX_END"))
    return "\n".join(lines) + "\n"


def main() -> None:
    for language, (extension, _) in LANGUAGES.items():
        for slot in ("alpha", "beta"):
            path = ROOT / language / f"registry_{slot}.{extension}"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(render(language, slot), encoding="utf-8")


if __name__ == "__main__":
    main()
