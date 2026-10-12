// Downloads brand glyphs from Simple Icons (CC0) into dist/assets/icons/.
// Each glyph is stored with fill #FFFFFF (rendered on a brand-colored chip);
// the brand color is recorded in dist/assets/icons/manifest.json.
// 404s are skipped — those services simply fall back to the monogram.
// Usage: node build-helpers/fetch-brand-icons.js

const fs = require("fs");
const path = require("path");

// slug list mirrors the recipe catalog + a few URL-match-only hosts
const SLUGS = [
  // messaging
  "whatsapp", "telegram", "discord", "slack", "messenger", "googlechat",
  "microsoftteams", "element", "mattermost", "rocketchat", "zulip", "wire",
  "threema", "wechat", "groupme", "skype", "steam", "zoom",
  // mail
  "gmail", "microsoftoutlook", "protonmail", "tuta", "tutanota", "fastmail",
  "yahoo",
  // social
  "x", "instagram", "linkedin", "facebook", "reddit", "mastodon", "bluesky",
  "tiktok", "pinterest", "twitch", "youtube",
  // ai
  "openai", "anthropic", "googlegemini", "perplexity",
  // productivity
  "notion", "googlecalendar", "googlekeep", "googledrive", "googledocs",
  "googlephotos", "onedrive", "dropbox", "trello", "asana", "todoist",
  "ticktick", "bitwarden",
  // url-match only (not in catalog)
  "github", "gitlab",
];

const outDir = path.join(__dirname, "..", "dist", "assets", "icons");
fs.mkdirSync(outDir, { recursive: true });

async function fetchColored(slug) {
  const res = await fetch(`https://cdn.simpleicons.org/${slug}`, {
    redirect: "follow",
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.text();
}

(async () => {
  const manifest = {};
  const failed = [];
  for (const slug of SLUGS) {
    try {
      const svg = await fetchColored(slug);
      const m = svg.match(/fill="(#[0-9a-fA-F]{6})"/i);
      if (!m) throw new Error("no fill color found");
      const color = m[1].toLowerCase();
      // store glyph in white; the UI paints the chip with the brand color
      const white = svg.replace(/fill="#[0-9a-fA-F]{6}"/gi, 'fill="#FFFFFF"');
      fs.writeFileSync(path.join(outDir, `${slug}.svg`), white);
      manifest[slug] = color;
      console.log(`ok   ${slug} (${color})`);
    } catch (e) {
      console.log(`skip ${slug}: ${e.message}`);
      failed.push(slug);
    }
  }
  fs.writeFileSync(path.join(outDir, "manifest.json"), JSON.stringify(manifest, null, 1));
  console.log(`\n${Object.keys(manifest).length} icons, ${failed.length} skipped: ${failed.join(", ") || "none"}`);
})();
