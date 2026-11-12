// A file attached to a GitHub issue or discussion. GitHub doesn't let other sites read one, so the
// Open page asks the reader to download it and choose it, rather than fetching it.

/** The attachment's file name, or null when `src` isn't a GitHub attachment's address. Only the
 * two forms GitHub writes are taken, whole: the page shouldn't hand readers any other download. */
export function githubAttachment(src: string): string | null {
  let url: URL;
  try {
    url = new URL(src);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" || url.hostname !== "github.com" || url.username || url.password || url.port || url.search || url.hash) return null;
  const m = /^\/(?:user-attachments|[\w.-]+\/[\w.-]+)\/files\/\d+\/([^/]+)$/.exec(url.pathname);
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]);
  } catch {
    return null;
  }
}
