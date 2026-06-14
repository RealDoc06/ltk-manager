export const WELL_KNOWN_TAGS = [
  "league-of-legends",
  "tft",
  "champion-skin",
  "map-skin",
  "ward-skin",
  "emote",
  "summoner-icon",
  "companion",
  "ui",
  "hud",
  "font",
  "sfx",
  "announcer",
  "structure",
  "minion",
  "jungle-monster",
  "misc",
];

export const WELL_KNOWN_MAPS = ["summoners-rift", "aram", "teamfight-tactics", "arena", "swarm"];

const TAG_LABELS: Record<string, string> = {
  "league-of-legends": "League of Legends",
  tft: "TFT",
  "champion-skin": "Champion Skin",
  "map-skin": "Map Skin",
  "ward-skin": "Ward Skin",
  emote: "Emote",
  "summoner-icon": "Summoner Icon",
  companion: "Companion",
  ui: "UI",
  hud: "HUD",
  font: "Font",
  sfx: "SFX",
  announcer: "Announcer",
  structure: "Structure",
  minion: "Minion",
  "jungle-monster": "Jungle Monster",
  misc: "Misc",
};

const MAP_LABELS: Record<string, string> = {
  "summoners-rift": "Summoner's Rift",
  aram: "ARAM",
  "teamfight-tactics": "Teamfight Tactics",
  arena: "Arena",
  swarm: "Swarm",
};

function kebabToTitleCase(s: string): string {
  return s
    .split("-")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

export function getTagLabel(tag: string): string {
  return TAG_LABELS[tag] ?? kebabToTitleCase(tag);
}

export function getMapLabel(map: string): string {
  return MAP_LABELS[map] ?? kebabToTitleCase(map);
}
