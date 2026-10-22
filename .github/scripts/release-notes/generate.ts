#!/usr/bin/env node

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { Command } from "commander";
import parseChangelog from "changelog-parser";
import { Octokit } from "@octokit/rest";
import OpenAI from "openai";
import semver from "semver";
import { truncate, uniqBy } from "lodash-es";

const options = new Command()
  .requiredOption("--tag <tag>", "release tag, for example v0.6.0", process.env.GITHUB_REF_NAME)
  .option("--previous-tag <tag>", "previous release tag")
  .option("--output <path>", "release notes output path", ".release/RELEASE_NOTES.md")
  .parse()
  .opts();

const tag = options.tag;
const version = tag.replace(/^v/, "");
const [owner, repoName] = process.env.GITHUB_REPOSITORY?.split("/") ?? [];
const repo = owner && repoName ? { owner, repo: repoName } : undefined;
const fullRepoName = repo ? `${repo.owner}/${repo.repo}` : undefined;
const octokit = new Octokit({ auth: process.env.GH_TOKEN || process.env.GITHUB_TOKEN });

const previousTag = options.previousTag ?? (repo ? await octokit.paginate(octokit.rest.repos.listTags, {
  owner: repo.owner,
  repo: repo.repo,
  per_page: 100,
})
  .then((tags) => tags
    .map(({ name }) => name)
    .filter((candidate) => semver.valid(candidate) && semver.lt(candidate, tag))
    .sort(semver.rcompare)[0] ?? "")
  .catch((error) => {
    console.warn(error.message);
    return "";
  }) : "");

const comparison = repo && previousTag ? await octokit.rest.repos
  .compareCommitsWithBasehead({
    owner: repo.owner,
    repo: repo.repo,
    basehead: `${previousTag}...${tag}`,
  })
  .then(({ data }) => data)
  .catch((error) => {
    console.warn(error.message);
    return undefined;
  }) : undefined;

const fullChangelog = comparison?.html_url;

const githubNotes = repo ? await octokit.rest.repos
  .generateReleaseNotes({
    owner: repo.owner,
    repo: repo.repo,
    tag_name: tag,
    previous_tag_name: previousTag || undefined,
  })
  .then(({ data }) => data.body ?? "")
  .catch((error) => {
    console.warn(error.message);
    return "";
  }) : "";

const pullRequests = repo && comparison ? uniqBy((await Promise.all(comparison.commits.slice(0, 50).map(({ sha }) => octokit.rest.repos
  .listPullRequestsAssociatedWithCommit({ owner: repo.owner, repo: repo.repo, commit_sha: sha })
  .then(({ data }) => data)
  .catch((error) => {
    console.warn(error.message);
    return [];
  })))).flat(), "number").slice(0, 20) : [];

const prDetails = pullRequests
  .map((pull) => [`#${pull.number} ${pull.title}`, pull.html_url, pull.body ?? ""].join("\n"))
  .join("\n\n---\n\n");

const commits = truncate(comparison?.commits
  .map(({ sha, commit }) => `${sha.slice(0, 7)} ${commit.message}`)
  .join("\n\n---\n\n") ?? "", { length: 80_000, omission: "\n\n[truncated]" });

const changelogSection = await parseChangelog({ filePath: "CHANGELOG.md", removeMarkdown: false })
  .then((changelog) => {
    const entry = changelog.versions.find((release) => release.version === version);
    return entry ? [`## ${entry.title}`, entry.body].filter(Boolean).join("\n\n") : "";
  })
  .catch((error) => {
    console.warn(error.message);
    return "";
  });

const prompt = `Generate polished GitHub Release notes for OpenLogi ${tag}.

Audience: end users and technical users installing OpenLogi.
Tone: concise, concrete, product-facing.

Required sections:
- Highlights
- What's new
- macOS packaging and startup
- Fixes and hardening
- Upgrade notes
- Included PRs
- Full changelog

Rules:
- Prefer user impact over implementation details.
- Mention macOS Accessibility, helper app, launch-at-login, signing/notarization, updater, or Homebrew behavior when relevant.
- Do not invent changes.
- If a required section has no meaningful entries, omit that section.
- Return only Markdown, with no surrounding code fence.

Repository: ${fullRepoName ?? "unknown"}
Previous tag: ${previousTag || "unknown"}
Current tag: ${tag}
Full changelog URL: ${fullChangelog ?? "unknown"}

GitHub generated notes:
${truncate(githubNotes, { length: 30_000, omission: "\n\n[truncated]" })}

CHANGELOG section:
${truncate(changelogSection, { length: 30_000, omission: "\n\n[truncated]" })}

PR details:
${truncate(prDetails, { length: 60_000, omission: "\n\n[truncated]" })}

Commit details:
${truncate(commits, { length: 80_000, omission: "\n\n[truncated]" })}
`;

/** Log why the LLM notes were skipped. Inside Actions the line is a warning
 * annotation, so it shows on the run summary instead of only in the step log.
 * The annotation ends at the first raw newline, so `%`, CR and LF are escaped
 * as the workflow command format requires. */
function warnUnavailable(reason: string) {
  const message = `Codex release notes unavailable: ${reason}`;
  if (process.env.GITHUB_ACTIONS !== "true") {
    console.warn(message);
    return;
  }
  const escaped = message.replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
  console.warn(`::warning title=Release notes::${escaped}`);
}

/** The expiry of a ChatGPT sign-in token, or `undefined` when `token` is not a
 * JWT (an OpenAI API key) or carries no `exp` claim. */
function jwtExpiry(token: string): Date | undefined {
  const parts = token.split(".");
  if (parts.length !== 3) return undefined;
  try {
    const claims = JSON.parse(Buffer.from(parts[1], "base64url").toString("utf8"));
    return typeof claims.exp === "number" ? new Date(claims.exp * 1000) : undefined;
  } catch {
    return undefined;
  }
}

let notes = "";
const codexAccessToken = process.env.CODEX_ACCESS_TOKEN || process.env.OPENAI_API_KEY;
// A ChatGPT sign-in token is a JWT and talks to the Codex backend; any other
// credential is an OpenAI API key and talks to the OpenAI API.
const chatgptToken = codexAccessToken?.split(".").length === 3;
const expiry = codexAccessToken ? jwtExpiry(codexAccessToken) : undefined;
if (!codexAccessToken) {
  warnUnavailable("CODEX_ACCESS_TOKEN is not set");
} else if (expiry && expiry.getTime() <= Date.now()) {
  warnUnavailable(`the ChatGPT token in CODEX_ACCESS_TOKEN expired on ${expiry.toISOString()}; replace it`);
} else {
  try {
    const response = await new OpenAI({
      apiKey: codexAccessToken,
      baseURL: process.env.CODEX_ENDPOINT
        || (chatgptToken ? "https://chatgpt.com/backend-api/codex" : "https://api.openai.com/v1"),
      defaultHeaders: chatgptToken && process.env.CHATGPT_USERNAME
        ? { "ChatGPT-Account-ID": process.env.CHATGPT_USERNAME }
        : undefined,
    }).responses.create({
      model: process.env.CODEX_MODEL || "gpt-5.5",
      input: prompt,
    });
    notes = response.output_text ?? "";
  } catch (error) {
    warnUnavailable(error instanceof Error ? error.message : String(error));
  }
}

if (!notes.trim()) {
  notes = githubNotes.trim()
    || (changelogSection.trim() && `${changelogSection}\n\n${fullChangelog ? `**Full changelog**: ${fullChangelog}` : ""}`)
    || [`## ${tag}`, "", commits.trim() || "No release notes were generated.", "", fullChangelog ? `**Full changelog**: ${fullChangelog}` : ""].join("\n");
}

mkdirSync(dirname(options.output), { recursive: true });
writeFileSync(options.output, `${notes.trim()}\n`);
console.log(`Wrote release notes to ${options.output}`);
