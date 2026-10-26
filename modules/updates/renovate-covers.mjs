// renovate-covers RENOVATE FILTERS OWNER/REPO: exits 0 when Renovate's
// autodiscoverFilter (FILTERS, JSON) takes the repo, 10 when it doesn't (and
// 1, node's own, when it can't tell).
// It asks Renovate's own code, the filter autodiscovery applies, so a run
// on one repo (renovate@) covers exactly the repos a full run does.
const [renovate, filters, repo] = process.argv.slice(2);
const { applyFilters } = await import(`${renovate}/lib/node_modules/renovate/dist/workers/global/autodiscover.js`);
process.exit(applyFilters([repo], JSON.parse(filters)).length === 1 ? 0 : 10);
