import { describe, expect, it } from "vitest";

import { countRegexMatches, isValidRegex, listRegexMatches } from "../wadRegex";

describe("isValidRegex", () => {
  it("accepts an empty pattern", () => {
    expect(isValidRegex("")).toBe(true);
  });

  it("accepts common anchored patterns", () => {
    expect(isValidRegex("^map\\d+\\.en_us\\.wad\\.client$")).toBe(true);
    expect(isValidRegex(".*")).toBe(true);
    expect(isValidRegex("[a-z]+")).toBe(true);
  });

  it("rejects an unterminated character class", () => {
    expect(isValidRegex("[bad(")).toBe(false);
  });

  it("rejects an unterminated group", () => {
    expect(isValidRegex("(abc")).toBe(false);
  });
});

describe("countRegexMatches", () => {
  const wads = [
    "aatrox.wad.client",
    "ahri.wad.client",
    "map11.en_us.wad.client",
    "map11.wad.client",
    "map22.en_us.wad.client",
    "map22.wad.client",
    "scripts.wad.client",
  ];

  it("returns 0 for an empty wad list", () => {
    expect(countRegexMatches(".*", [])).toBe(0);
  });

  it("returns 0 for an invalid pattern instead of throwing", () => {
    expect(countRegexMatches("[bad(", wads)).toBe(0);
  });

  it("counts anchored matches", () => {
    expect(countRegexMatches("^map\\d+\\.en_us\\.wad\\.client$", wads)).toBe(2);
  });

  it("counts all map wads via a loose pattern", () => {
    expect(countRegexMatches("^map", wads)).toBe(4);
  });

  it("matches case-insensitively", () => {
    expect(countRegexMatches("AATROX", wads)).toBe(1);
    expect(countRegexMatches("^MAP\\d+$", ["MAP11", "map22", "other"])).toBe(2);
  });

  it("returns 1 for a pattern matching exactly one file", () => {
    expect(countRegexMatches("^scripts", wads)).toBe(1);
  });

  it("returns the total for .*", () => {
    expect(countRegexMatches(".*", wads)).toBe(wads.length);
  });
});

describe("listRegexMatches", () => {
  const wads = [
    "aatrox.wad.client",
    "ahri.wad.client",
    "map11.en_us.wad.client",
    "map11.wad.client",
    "map22.en_us.wad.client",
    "map22.wad.client",
    "scripts.wad.client",
  ];

  it("returns [] for an empty wad list", () => {
    expect(listRegexMatches(".*", [])).toEqual([]);
  });

  it("returns [] for an invalid pattern instead of throwing", () => {
    expect(listRegexMatches("[bad(", wads)).toEqual([]);
  });

  it("returns the matching wads, preserving input order", () => {
    expect(listRegexMatches("^map\\d+\\.en_us\\.wad\\.client$", wads)).toEqual([
      "map11.en_us.wad.client",
      "map22.en_us.wad.client",
    ]);
  });

  it("matches case-insensitively", () => {
    expect(listRegexMatches("AATROX", wads)).toEqual(["aatrox.wad.client"]);
  });

  it("agrees with countRegexMatches", () => {
    for (const pattern of ["^map", "^scripts", ".*", "[bad("]) {
      expect(listRegexMatches(pattern, wads)).toHaveLength(countRegexMatches(pattern, wads));
    }
  });
});
