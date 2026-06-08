#!/usr/bin/env node

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { Command } from "commander";
import parseChangelog from "changelog-parser";
import { Octokit } from "@octokit/rest";
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

let notes = "";
if (process.env.AMP_API_KEY) {
  try {
    const { execute } = await import("@ampcode/sdk");
    for await (const message of execute({ prompt, options: { cwd: process.cwd() } })) {
      if (message.type === "result") {
        if (message.is_error) throw new Error(message.error ?? "Amp returned an error");
        notes = message.result ?? "";
      }
    }
  } catch (error) {
    console.warn(`AI release notes unavailable: ${error.message}`);
  }
} else {
  console.warn("AI release notes unavailable: AMP_API_KEY is not set");
}

if (!notes.trim()) {
  notes = githubNotes.trim()
    || (changelogSection.trim() && `${changelogSection}\n\n${fullChangelog ? `**Full changelog**: ${fullChangelog}` : ""}`)
    || [`## ${tag}`, "", commits.trim() || "No release notes were generated.", "", fullChangelog ? `**Full changelog**: ${fullChangelog}` : ""].join("\n");
}

mkdirSync(dirname(options.output), { recursive: true });
writeFileSync(options.output, `${notes.trim()}\n`);
console.log(`Wrote release notes to ${options.output}`);
