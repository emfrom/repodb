// REPODB_SYNTAX_BEGIN
using System;
using System.Collections.Generic;
record RepodbCsharpBetaEntityV1(int Id, string Name, bool Active = true);
record AuditEvent(int Id, string Action);
class Registry {
  readonly Dictionary<int, object> entities = new();
  readonly List<AuditEvent> events = new();
  public void Add(int id, object value) { entities[id] = value; events.Add(new(id, "create")); }
  public object? Find(int id) => entities.GetValueOrDefault(id);
  public void Archive(int id) { entities.Remove(id); events.Add(new(id, "archive")); }
}
static class Seed001 {
  public static RepodbCsharpBetaEntityV1 Create() => new(1, "record-001");
}
static class Seed002 {
  public static RepodbCsharpBetaEntityV1 Create() => new(2, "record-002");
}
static class Seed003 {
  public static RepodbCsharpBetaEntityV1 Create() => new(3, "record-003");
}
static class Seed004 {
  public static RepodbCsharpBetaEntityV1 Create() => new(4, "record-004");
}
static class Seed005 {
  public static RepodbCsharpBetaEntityV1 Create() => new(5, "record-005");
}
static class Seed006 {
  public static RepodbCsharpBetaEntityV1 Create() => new(6, "record-006");
}
static class Seed007 {
  public static RepodbCsharpBetaEntityV1 Create() => new(7, "record-007");
}
static class Seed008 {
  public static RepodbCsharpBetaEntityV1 Create() => new(8, "record-008");
}
static class Seed009 {
  public static RepodbCsharpBetaEntityV1 Create() => new(9, "record-009");
}
static class Seed010 {
  public static RepodbCsharpBetaEntityV1 Create() => new(10, "record-010");
}
static class Seed011 {
  public static RepodbCsharpBetaEntityV1 Create() => new(11, "record-011");
}
static class Seed012 {
  public static RepodbCsharpBetaEntityV1 Create() => new(12, "record-012");
}
static class Seed013 {
  public static RepodbCsharpBetaEntityV1 Create() => new(13, "record-013");
}
static class Seed014 {
  public static RepodbCsharpBetaEntityV1 Create() => new(14, "record-014");
}
static class Seed015 {
  public static RepodbCsharpBetaEntityV1 Create() => new(15, "record-015");
}
static class Seed016 {
  public static RepodbCsharpBetaEntityV1 Create() => new(16, "record-016");
}
static class Seed017 {
  public static RepodbCsharpBetaEntityV1 Create() => new(17, "record-017");
}
static class Seed018 {
  public static RepodbCsharpBetaEntityV1 Create() => new(18, "record-018");
}
static class Seed019 {
  public static RepodbCsharpBetaEntityV1 Create() => new(19, "record-019");
}
static class Seed020 {
  public static RepodbCsharpBetaEntityV1 Create() => new(20, "record-020");
}
static class Seed021 {
  public static RepodbCsharpBetaEntityV1 Create() => new(21, "record-021");
}
static class Seed022 {
  public static RepodbCsharpBetaEntityV1 Create() => new(22, "record-022");
}
static class Seed023 {
  public static RepodbCsharpBetaEntityV1 Create() => new(23, "record-023");
}
static class Seed024 {
  public static RepodbCsharpBetaEntityV1 Create() => new(24, "record-024");
}
static class Seed025 {
  public static RepodbCsharpBetaEntityV1 Create() => new(25, "record-025");
}
static class Seed026 {
  public static RepodbCsharpBetaEntityV1 Create() => new(26, "record-026");
}
static class Seed027 {
  public static RepodbCsharpBetaEntityV1 Create() => new(27, "record-027");
}
static class Seed028 {
  public static RepodbCsharpBetaEntityV1 Create() => new(28, "record-028");
}
static class Seed029 {
  public static RepodbCsharpBetaEntityV1 Create() => new(29, "record-029");
}
static class Seed030 {
  public static RepodbCsharpBetaEntityV1 Create() => new(30, "record-030");
}
static class Seed031 {
  public static RepodbCsharpBetaEntityV1 Create() => new(31, "record-031");
}
static class Seed032 {
  public static RepodbCsharpBetaEntityV1 Create() => new(32, "record-032");
}
static class Seed033 {
  public static RepodbCsharpBetaEntityV1 Create() => new(33, "record-033");
}
static class Seed034 {
  public static RepodbCsharpBetaEntityV1 Create() => new(34, "record-034");
}
static class Seed035 {
  public static RepodbCsharpBetaEntityV1 Create() => new(35, "record-035");
}
static class Seed036 {
  public static RepodbCsharpBetaEntityV1 Create() => new(36, "record-036");
}
static class Seed037 {
  public static RepodbCsharpBetaEntityV1 Create() => new(37, "record-037");
}
static class Seed038 {
  public static RepodbCsharpBetaEntityV1 Create() => new(38, "record-038");
}
static class Seed039 {
  public static RepodbCsharpBetaEntityV1 Create() => new(39, "record-039");
}
static class Seed040 {
  public static RepodbCsharpBetaEntityV1 Create() => new(40, "record-040");
}
static class Seed041 {
  public static RepodbCsharpBetaEntityV1 Create() => new(41, "record-041");
}
static class Seed042 {
  public static RepodbCsharpBetaEntityV1 Create() => new(42, "record-042");
}
static class Seed043 {
  public static RepodbCsharpBetaEntityV1 Create() => new(43, "record-043");
}
static class Seed044 {
  public static RepodbCsharpBetaEntityV1 Create() => new(44, "record-044");
}
static class Seed045 {
  public static RepodbCsharpBetaEntityV1 Create() => new(45, "record-045");
}
static class Seed046 {
  public static RepodbCsharpBetaEntityV1 Create() => new(46, "record-046");
}
static class Seed047 {
  public static RepodbCsharpBetaEntityV1 Create() => new(47, "record-047");
}
static class Seed048 {
  public static RepodbCsharpBetaEntityV1 Create() => new(48, "record-048");
}
static class Seed049 {
  public static RepodbCsharpBetaEntityV1 Create() => new(49, "record-049");
}
static class Seed050 {
  public static RepodbCsharpBetaEntityV1 Create() => new(50, "record-050");
}
static class Seed051 {
  public static RepodbCsharpBetaEntityV1 Create() => new(51, "record-051");
}
static class Seed052 {
  public static RepodbCsharpBetaEntityV1 Create() => new(52, "record-052");
}
static class Seed053 {
  public static RepodbCsharpBetaEntityV1 Create() => new(53, "record-053");
}
static class Seed054 {
  public static RepodbCsharpBetaEntityV1 Create() => new(54, "record-054");
}
static class Seed055 {
  public static RepodbCsharpBetaEntityV1 Create() => new(55, "record-055");
}
static class Seed056 {
  public static RepodbCsharpBetaEntityV1 Create() => new(56, "record-056");
}
static class Seed057 {
  public static RepodbCsharpBetaEntityV1 Create() => new(57, "record-057");
}
static class Seed058 {
  public static RepodbCsharpBetaEntityV1 Create() => new(58, "record-058");
}
static class Seed059 {
  public static RepodbCsharpBetaEntityV1 Create() => new(59, "record-059");
}
static class Seed060 {
  public static RepodbCsharpBetaEntityV1 Create() => new(60, "record-060");
}
class Program {
  static void Main() {
    var registry = new Registry();
    registry.Add(1, Seed001.Create());
    registry.Find(1);
    registry.Archive(1);
  }
}
// REPODB_SYNTAX_END
