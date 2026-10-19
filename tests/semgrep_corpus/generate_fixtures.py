#!/usr/bin/env python3
"""Regenerate the small, independent Semgrep v1.181.0 parser fixtures.

Every fixture has a unique V1 marker; tests change only V1 to V2 in the
second commit. No fixture implements a shared program or application domain.
"""
from pathlib import Path

ROOT = Path(__file__).parent / "sources"

# The first 24 entries follow the 2026 Stack Overflow survey's used ordering
# after intersecting with the pinned OSS parser inventory. Move is supplemental.
SOURCES = {
    "javascript": ("js", "const RepodbJavascriptSampleV1 = 7;\nfunction double(value) { return value * 2; }\n"),
    "html": ("html", "<section id=\"RepodbHtmlSampleV1\">Hello</section>\n<aside>Secondary</aside>\n"),
    "python": ("py", "def RepodbPythonSampleV1(value):\n    return value + 1\ndef square(value):\n    return value * value\n"),
    "bash": ("sh", "RepodbBashSampleV1=7\ndouble() { echo \"$(( $1 * 2 ))\"; }\n"),
    "typescript": ("ts", "const RepodbTypescriptSampleV1: number = 7;\nfunction double(value: number): number { return value * 2; }\n"),
    "java": ("java", "class RepodbJavaSampleV1 { int value() { return 7; } }\nclass Secondary { int twice(int n) { return n * 2; } }\n"),
    "csharp": ("cs", "class RepodbCsharpSampleV1 { public int Value() => 7; }\nclass Secondary { public int Twice(int n) => n * 2; }\n"),
    "cpp": ("cpp", "int RepodbCppSampleV1() { return 7; }\nint twice(int value) { return value * 2; }\n"),
    "c": ("c", "int RepodbCSampleV1(void) { return 7; }\nint twice(int value) { return value * 2; }\n"),
    "rust": ("rs", "fn RepodbRustSampleV1() -> i32 { 7 }\nfn twice(value: i32) -> i32 { value * 2 }\n"),
    "php": ("php", "<?php\nfunction RepodbPhpSampleV1(): int { return 7; }\nfunction twice(int $n): int { return $n * 2; }\n"),
    "go": ("go", "package example\nfunc RepodbGoSampleV1() int { return 7 }\nfunc twice(value int) int { return value * 2 }\n"),
    "kotlin": ("kt", "fun RepodbKotlinSampleV1(): Int = 7\nfun twice(value: Int): Int = value * 2\n"),
    "lua": ("lua", "function RepodbLuaSampleV1() return 7 end\nfunction twice(value) return value * 2 end\n"),
    "swift": ("swift", "func RepodbSwiftSampleV1() -> Int { return 7 }\nfunc twice(_ value: Int) -> Int { return value * 2 }\n"),
    "ruby": ("rb", "def RepodbRubySampleV1\n  7\nend\ndef twice(value)\n  value * 2\nend\n"),
    "dart": ("dart", "int RepodbDartSampleV1() => 7;\nint twice(int value) => value * 2;\n"),
    "r": ("r", "RepodbRSampleV1 <- function() 7\ntwice <- function(value) value * 2\n"),
    "lisp": ("lisp", "(defun RepodbLispSampleV1 () 7)\n(defun twice (value) (* value 2))\n"),
    "scala": ("scala", "object RepodbScalaSampleV1 { def value: Int = 7 }\nobject Secondary { def twice(value: Int): Int = value * 2 }\n"),
    "julia": ("jl", "RepodbJuliaSampleV1() = 7\ntwice(value) = value * 2\n"),
    "clojure": ("clj", "(ns repodb.sample)\n(defn RepodbClojureSampleV1 [] 7)\n(defn twice [value] (* value 2))\n"),
    "ocaml": ("ml", "let RepodbOcamlSampleV1 () = 7\nlet twice value = value * 2\n"),
    "solidity": ("sol", "pragma solidity ^0.8.20;\ncontract RepodbSoliditySampleV1 { function value() public pure returns (uint) { return 7; } }\ncontract Secondary { function twice(uint n) public pure returns (uint) { return n * 2; } }\n"),
    "move_sui": ("move", "module 0x1::RepodbMoveSuiSampleV1 { public fun value(): u64 { 7 } }\n"),
    "move_aptos": ("move", "module 0x1::RepodbMoveAptosSampleV1 { public fun value(): u64 { 7 } }\n"),
}


def main() -> None:
    for language, (extension, source) in SOURCES.items():
        path = ROOT / language / f"sample.{extension}"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")


if __name__ == "__main__":
    main()
