# Wiqo local browser worker

The companion bundles the worker and its pinned Playwright package manifest. On the first user-initiated browser task, the worker installs the locked Playwright dependency and Chromium into the current user's application-support directory. Install scripts are disabled; downloads use the package lock and Playwright's official browser installer. Later tasks reuse the installed runtime.

Node.js 20 or newer and an internet connection are required for first-use setup. The setup runs only when the browser capability is used, has a 105-second bound, and returns a safe actionable error if a prerequisite is missing or a download cannot finish. The browser and package cache stay under the current user's application-support directory; no personal browser profile is accessed.

The worker starts only for a user-initiated browser task. Each task gets a temporary browser context; closing or expiring the task removes its cookies and page state.

The companion exposes only accessible controls discovered by `observe`. Each control receives a
short task-local semantic reference; models never send CSS selectors or JavaScript. A reference stays
stable while its role, accessible name and page identity stay stable. `observe_diff` returns changed
controls, and `run_steps` executes at most eight validated actions under the same task grant, stopping
at the first failure and returning per-step evidence. Password, OTP, payment, upload, login and
messaging actions remain blocked.

Closing a command or finishing a task never closes the visible browser. The task context and browser
are closed only when the user explicitly asks to close them or the desktop application shuts down.

## Verified document downloads

`download` is a standalone action on an observed target under the existing browser grant. Public document links stream into a task-specific subdirectory of the worker's application-data directory. Each redirect is revalidated and DNS is pinned to the allowed public address. Streams are capped at 25 MB and 25 seconds; cancellation aborts the stream. Chromium's own downloads remain disabled, including extra popup downloads. Existing filenames are never overwritten, temporary files are cleaned, and the saved bytes are read back to verify SHA-256 before returning a local path.

Only PDF, DOCX, XLSX, PPTX, CSV, TXT and Markdown are accepted. No file is launched. HTML login/error responses and obvious type mismatches fail. Authenticated or blob-only downloads may require a manual download; browser cookies are not copied to the HTTP adapter. Read the returned path through the existing document/file capability. Download is excluded from `run_steps` so its receipt cannot be lost behind a later step. Saved documents remain until the user removes them; closing the browser does not remove their outputs.

Run focused verification with `node --test RovaBrowserWorker/downloads.test.mjs`.
