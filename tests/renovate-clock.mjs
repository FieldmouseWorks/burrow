// The clock a test of Renovate runs on, so its answer doesn't hang on when
// or where it runs: Renovate holds a branch to the config's schedule by
// luxon's now, and ages a release by `new Date()`. Importing this pins
// both, and the zone a config without a timezone is read in, to UTC at the
// instant `at` last set: a night in the window the policy's schedule
// gives, unless a test moves it.
process.env.TZ = "UTC";
const Real = Date;
let now = Real.parse("2026-11-06T01:30:00Z");
globalThis.Date = class extends Real {
  constructor(...args) {
    super(...(args.length ? args : [now]));
  }
  static now() {
    return now;
  }
};
export const at = (iso) => {
  now = Real.parse(iso);
};
