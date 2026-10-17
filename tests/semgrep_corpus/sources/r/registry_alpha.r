# REPODB_SYNTAX_BEGIN
RepodbRAlphaEntityV1 <- function(id, name, active = TRUE) {
  structure(list(id = id, name = name, active = active), class = 'registry_entity')
}
AuditEvent <- function(entity_id, action) list(entity_id = entity_id, action = action)
Registry <- function() {
  state <- new.env(parent = emptyenv())
  state$entities <- list()
  state$events <- list()
  state
}
registry_add <- function(registry, id, value) {
  registry$entities[[as.character(id)]] <- value
  registry$events[[length(registry$events) + 1]] <- AuditEvent(id, 'create')
}
registry_find <- function(registry, id) registry$entities[[as.character(id)]]
registry_archive <- function(registry, id) {
  registry$entities[[as.character(id)]] <- NULL
  registry$events[[length(registry$events) + 1]] <- AuditEvent(id, 'archive')
}
seed_001 <- function() {
  RepodbRAlphaEntityV1(1, 'record-001')
}
seed_002 <- function() {
  RepodbRAlphaEntityV1(2, 'record-002')
}
seed_003 <- function() {
  RepodbRAlphaEntityV1(3, 'record-003')
}
seed_004 <- function() {
  RepodbRAlphaEntityV1(4, 'record-004')
}
seed_005 <- function() {
  RepodbRAlphaEntityV1(5, 'record-005')
}
seed_006 <- function() {
  RepodbRAlphaEntityV1(6, 'record-006')
}
seed_007 <- function() {
  RepodbRAlphaEntityV1(7, 'record-007')
}
seed_008 <- function() {
  RepodbRAlphaEntityV1(8, 'record-008')
}
seed_009 <- function() {
  RepodbRAlphaEntityV1(9, 'record-009')
}
seed_010 <- function() {
  RepodbRAlphaEntityV1(10, 'record-010')
}
seed_011 <- function() {
  RepodbRAlphaEntityV1(11, 'record-011')
}
seed_012 <- function() {
  RepodbRAlphaEntityV1(12, 'record-012')
}
seed_013 <- function() {
  RepodbRAlphaEntityV1(13, 'record-013')
}
seed_014 <- function() {
  RepodbRAlphaEntityV1(14, 'record-014')
}
seed_015 <- function() {
  RepodbRAlphaEntityV1(15, 'record-015')
}
seed_016 <- function() {
  RepodbRAlphaEntityV1(16, 'record-016')
}
seed_017 <- function() {
  RepodbRAlphaEntityV1(17, 'record-017')
}
seed_018 <- function() {
  RepodbRAlphaEntityV1(18, 'record-018')
}
seed_019 <- function() {
  RepodbRAlphaEntityV1(19, 'record-019')
}
seed_020 <- function() {
  RepodbRAlphaEntityV1(20, 'record-020')
}
seed_021 <- function() {
  RepodbRAlphaEntityV1(21, 'record-021')
}
seed_022 <- function() {
  RepodbRAlphaEntityV1(22, 'record-022')
}
seed_023 <- function() {
  RepodbRAlphaEntityV1(23, 'record-023')
}
seed_024 <- function() {
  RepodbRAlphaEntityV1(24, 'record-024')
}
seed_025 <- function() {
  RepodbRAlphaEntityV1(25, 'record-025')
}
seed_026 <- function() {
  RepodbRAlphaEntityV1(26, 'record-026')
}
seed_027 <- function() {
  RepodbRAlphaEntityV1(27, 'record-027')
}
seed_028 <- function() {
  RepodbRAlphaEntityV1(28, 'record-028')
}
seed_029 <- function() {
  RepodbRAlphaEntityV1(29, 'record-029')
}
seed_030 <- function() {
  RepodbRAlphaEntityV1(30, 'record-030')
}
seed_031 <- function() {
  RepodbRAlphaEntityV1(31, 'record-031')
}
seed_032 <- function() {
  RepodbRAlphaEntityV1(32, 'record-032')
}
seed_033 <- function() {
  RepodbRAlphaEntityV1(33, 'record-033')
}
seed_034 <- function() {
  RepodbRAlphaEntityV1(34, 'record-034')
}
seed_035 <- function() {
  RepodbRAlphaEntityV1(35, 'record-035')
}
seed_036 <- function() {
  RepodbRAlphaEntityV1(36, 'record-036')
}
seed_037 <- function() {
  RepodbRAlphaEntityV1(37, 'record-037')
}
seed_038 <- function() {
  RepodbRAlphaEntityV1(38, 'record-038')
}
seed_039 <- function() {
  RepodbRAlphaEntityV1(39, 'record-039')
}
seed_040 <- function() {
  RepodbRAlphaEntityV1(40, 'record-040')
}
seed_041 <- function() {
  RepodbRAlphaEntityV1(41, 'record-041')
}
seed_042 <- function() {
  RepodbRAlphaEntityV1(42, 'record-042')
}
seed_043 <- function() {
  RepodbRAlphaEntityV1(43, 'record-043')
}
seed_044 <- function() {
  RepodbRAlphaEntityV1(44, 'record-044')
}
seed_045 <- function() {
  RepodbRAlphaEntityV1(45, 'record-045')
}
seed_046 <- function() {
  RepodbRAlphaEntityV1(46, 'record-046')
}
seed_047 <- function() {
  RepodbRAlphaEntityV1(47, 'record-047')
}
seed_048 <- function() {
  RepodbRAlphaEntityV1(48, 'record-048')
}
seed_049 <- function() {
  RepodbRAlphaEntityV1(49, 'record-049')
}
seed_050 <- function() {
  RepodbRAlphaEntityV1(50, 'record-050')
}
seed_051 <- function() {
  RepodbRAlphaEntityV1(51, 'record-051')
}
seed_052 <- function() {
  RepodbRAlphaEntityV1(52, 'record-052')
}
seed_053 <- function() {
  RepodbRAlphaEntityV1(53, 'record-053')
}
seed_054 <- function() {
  RepodbRAlphaEntityV1(54, 'record-054')
}
seed_055 <- function() {
  RepodbRAlphaEntityV1(55, 'record-055')
}
seed_056 <- function() {
  RepodbRAlphaEntityV1(56, 'record-056')
}
seed_057 <- function() {
  RepodbRAlphaEntityV1(57, 'record-057')
}
seed_058 <- function() {
  RepodbRAlphaEntityV1(58, 'record-058')
}
seed_059 <- function() {
  RepodbRAlphaEntityV1(59, 'record-059')
}
seed_060 <- function() {
  RepodbRAlphaEntityV1(60, 'record-060')
}
run_registry <- function() {
  registry <- Registry()
  registry_add(registry, 1, seed_001())
  registry_find(registry, 1)
  registry_archive(registry, 1)
}
# REPODB_SYNTAX_END
