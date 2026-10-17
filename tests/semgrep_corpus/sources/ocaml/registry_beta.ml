(* REPODB_SYNTAX_BEGIN *)
type repodb_ocaml_beta_entity_v1 = { id : int; name : string; active : bool }
type entity = repodb_ocaml_beta_entity_v1
type audit_event = { entity_id : int; action : string }
type registry = { mutable entities : entity list; mutable events : audit_event list }
let empty_registry () = { entities = []; events = [] }
let add registry entity =
  registry.entities <- entity :: registry.entities;
  registry.events <- { entity_id = entity.id; action = "create" } :: registry.events
let find registry id = List.find_opt (fun entity -> entity.id = id) registry.entities
let archive registry id =
  registry.entities <- List.filter (fun entity -> entity.id <> id) registry.entities;
  registry.events <- { entity_id = id; action = "archive" } :: registry.events
let seed_001 () =
  ({ id = 1; name = "record-001"; active = true } : entity)
let seed_002 () =
  ({ id = 2; name = "record-002"; active = true } : entity)
let seed_003 () =
  ({ id = 3; name = "record-003"; active = true } : entity)
let seed_004 () =
  ({ id = 4; name = "record-004"; active = true } : entity)
let seed_005 () =
  ({ id = 5; name = "record-005"; active = true } : entity)
let seed_006 () =
  ({ id = 6; name = "record-006"; active = true } : entity)
let seed_007 () =
  ({ id = 7; name = "record-007"; active = true } : entity)
let seed_008 () =
  ({ id = 8; name = "record-008"; active = true } : entity)
let seed_009 () =
  ({ id = 9; name = "record-009"; active = true } : entity)
let seed_010 () =
  ({ id = 10; name = "record-010"; active = true } : entity)
let seed_011 () =
  ({ id = 11; name = "record-011"; active = true } : entity)
let seed_012 () =
  ({ id = 12; name = "record-012"; active = true } : entity)
let seed_013 () =
  ({ id = 13; name = "record-013"; active = true } : entity)
let seed_014 () =
  ({ id = 14; name = "record-014"; active = true } : entity)
let seed_015 () =
  ({ id = 15; name = "record-015"; active = true } : entity)
let seed_016 () =
  ({ id = 16; name = "record-016"; active = true } : entity)
let seed_017 () =
  ({ id = 17; name = "record-017"; active = true } : entity)
let seed_018 () =
  ({ id = 18; name = "record-018"; active = true } : entity)
let seed_019 () =
  ({ id = 19; name = "record-019"; active = true } : entity)
let seed_020 () =
  ({ id = 20; name = "record-020"; active = true } : entity)
let seed_021 () =
  ({ id = 21; name = "record-021"; active = true } : entity)
let seed_022 () =
  ({ id = 22; name = "record-022"; active = true } : entity)
let seed_023 () =
  ({ id = 23; name = "record-023"; active = true } : entity)
let seed_024 () =
  ({ id = 24; name = "record-024"; active = true } : entity)
let seed_025 () =
  ({ id = 25; name = "record-025"; active = true } : entity)
let seed_026 () =
  ({ id = 26; name = "record-026"; active = true } : entity)
let seed_027 () =
  ({ id = 27; name = "record-027"; active = true } : entity)
let seed_028 () =
  ({ id = 28; name = "record-028"; active = true } : entity)
let seed_029 () =
  ({ id = 29; name = "record-029"; active = true } : entity)
let seed_030 () =
  ({ id = 30; name = "record-030"; active = true } : entity)
let seed_031 () =
  ({ id = 31; name = "record-031"; active = true } : entity)
let seed_032 () =
  ({ id = 32; name = "record-032"; active = true } : entity)
let seed_033 () =
  ({ id = 33; name = "record-033"; active = true } : entity)
let seed_034 () =
  ({ id = 34; name = "record-034"; active = true } : entity)
let seed_035 () =
  ({ id = 35; name = "record-035"; active = true } : entity)
let seed_036 () =
  ({ id = 36; name = "record-036"; active = true } : entity)
let seed_037 () =
  ({ id = 37; name = "record-037"; active = true } : entity)
let seed_038 () =
  ({ id = 38; name = "record-038"; active = true } : entity)
let seed_039 () =
  ({ id = 39; name = "record-039"; active = true } : entity)
let seed_040 () =
  ({ id = 40; name = "record-040"; active = true } : entity)
let seed_041 () =
  ({ id = 41; name = "record-041"; active = true } : entity)
let seed_042 () =
  ({ id = 42; name = "record-042"; active = true } : entity)
let seed_043 () =
  ({ id = 43; name = "record-043"; active = true } : entity)
let seed_044 () =
  ({ id = 44; name = "record-044"; active = true } : entity)
let seed_045 () =
  ({ id = 45; name = "record-045"; active = true } : entity)
let seed_046 () =
  ({ id = 46; name = "record-046"; active = true } : entity)
let seed_047 () =
  ({ id = 47; name = "record-047"; active = true } : entity)
let seed_048 () =
  ({ id = 48; name = "record-048"; active = true } : entity)
let seed_049 () =
  ({ id = 49; name = "record-049"; active = true } : entity)
let seed_050 () =
  ({ id = 50; name = "record-050"; active = true } : entity)
let seed_051 () =
  ({ id = 51; name = "record-051"; active = true } : entity)
let seed_052 () =
  ({ id = 52; name = "record-052"; active = true } : entity)
let seed_053 () =
  ({ id = 53; name = "record-053"; active = true } : entity)
let seed_054 () =
  ({ id = 54; name = "record-054"; active = true } : entity)
let seed_055 () =
  ({ id = 55; name = "record-055"; active = true } : entity)
let seed_056 () =
  ({ id = 56; name = "record-056"; active = true } : entity)
let seed_057 () =
  ({ id = 57; name = "record-057"; active = true } : entity)
let seed_058 () =
  ({ id = 58; name = "record-058"; active = true } : entity)
let seed_059 () =
  ({ id = 59; name = "record-059"; active = true } : entity)
let seed_060 () =
  ({ id = 60; name = "record-060"; active = true } : entity)
let seed_061 () =
  ({ id = 61; name = "record-061"; active = true } : entity)
let seed_062 () =
  ({ id = 62; name = "record-062"; active = true } : entity)
let seed_063 () =
  ({ id = 63; name = "record-063"; active = true } : entity)
let seed_064 () =
  ({ id = 64; name = "record-064"; active = true } : entity)
let seed_065 () =
  ({ id = 65; name = "record-065"; active = true } : entity)
let seed_066 () =
  ({ id = 66; name = "record-066"; active = true } : entity)
let seed_067 () =
  ({ id = 67; name = "record-067"; active = true } : entity)
let seed_068 () =
  ({ id = 68; name = "record-068"; active = true } : entity)
let seed_069 () =
  ({ id = 69; name = "record-069"; active = true } : entity)
let seed_070 () =
  ({ id = 70; name = "record-070"; active = true } : entity)
let seed_071 () =
  ({ id = 71; name = "record-071"; active = true } : entity)
let seed_072 () =
  ({ id = 72; name = "record-072"; active = true } : entity)
let seed_073 () =
  ({ id = 73; name = "record-073"; active = true } : entity)
let seed_074 () =
  ({ id = 74; name = "record-074"; active = true } : entity)
let seed_075 () =
  ({ id = 75; name = "record-075"; active = true } : entity)
let seed_076 () =
  ({ id = 76; name = "record-076"; active = true } : entity)
let seed_077 () =
  ({ id = 77; name = "record-077"; active = true } : entity)
let seed_078 () =
  ({ id = 78; name = "record-078"; active = true } : entity)
let seed_079 () =
  ({ id = 79; name = "record-079"; active = true } : entity)
let seed_080 () =
  ({ id = 80; name = "record-080"; active = true } : entity)
let seed_081 () =
  ({ id = 81; name = "record-081"; active = true } : entity)
let seed_082 () =
  ({ id = 82; name = "record-082"; active = true } : entity)
let seed_083 () =
  ({ id = 83; name = "record-083"; active = true } : entity)
let seed_084 () =
  ({ id = 84; name = "record-084"; active = true } : entity)
let seed_085 () =
  ({ id = 85; name = "record-085"; active = true } : entity)
let seed_086 () =
  ({ id = 86; name = "record-086"; active = true } : entity)
let seed_087 () =
  ({ id = 87; name = "record-087"; active = true } : entity)
let seed_088 () =
  ({ id = 88; name = "record-088"; active = true } : entity)
let seed_089 () =
  ({ id = 89; name = "record-089"; active = true } : entity)
let seed_090 () =
  ({ id = 90; name = "record-090"; active = true } : entity)
let seed_091 () =
  ({ id = 91; name = "record-091"; active = true } : entity)
let seed_092 () =
  ({ id = 92; name = "record-092"; active = true } : entity)
let seed_093 () =
  ({ id = 93; name = "record-093"; active = true } : entity)
let seed_094 () =
  ({ id = 94; name = "record-094"; active = true } : entity)
let seed_095 () =
  ({ id = 95; name = "record-095"; active = true } : entity)
let seed_096 () =
  ({ id = 96; name = "record-096"; active = true } : entity)
let seed_097 () =
  ({ id = 97; name = "record-097"; active = true } : entity)
let seed_098 () =
  ({ id = 98; name = "record-098"; active = true } : entity)
let seed_099 () =
  ({ id = 99; name = "record-099"; active = true } : entity)
let seed_100 () =
  ({ id = 100; name = "record-100"; active = true } : entity)
let seed_101 () =
  ({ id = 101; name = "record-101"; active = true } : entity)
let seed_102 () =
  ({ id = 102; name = "record-102"; active = true } : entity)
let seed_103 () =
  ({ id = 103; name = "record-103"; active = true } : entity)
let seed_104 () =
  ({ id = 104; name = "record-104"; active = true } : entity)
let seed_105 () =
  ({ id = 105; name = "record-105"; active = true } : entity)
let run_registry () =
  let registry = empty_registry () in
  add registry (seed_001 ());
  ignore (find registry 1);
  archive registry 1
(* REPODB_SYNTAX_END *)
