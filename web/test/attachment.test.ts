import { test } from "node:test";
import assert from "node:assert/strict";
import { githubAttachment } from "../../site/attachment.ts";

test("GitHub's two attachment addresses give the file's name", () => {
  assert.equal(githubAttachment("https://github.com/user-attachments/files/30440851/swd%20reads.zip"), "swd reads.zip");
  assert.equal(githubAttachment("https://github.com/Escapement-Project/verge/files/123/capture.zip"), "capture.zip");
  assert.equal(githubAttachment("https://GitHub.com/user-attachments/files/1/a.zip"), "a.zip");
});

test("an address that only looks like one is not taken", () => {
  for (const src of [
    "https://github.com.evil.example/user-attachments/files/1/a.zip",
    "https://github.com@evil.example/user-attachments/files/1/a.zip",
    "https://user@github.com/user-attachments/files/1/a.zip",
    "https://github.com:8443/user-attachments/files/1/a.zip",
    "http://github.com/user-attachments/files/1/a.zip",
    "https://github.com./user-attachments/files/1/a.zip",
    "https://evil.example/https://github.com/user-attachments/files/1/a.zip",
    "https://github.com/user-attachments/files/1/a.zip?x=1",
    "https://github.com/user-attachments/files/1/a.zip#x",
    "https://github.com/user-attachments/files/x/a.zip",
    "https://github.com/user-attachments/files/1/sub/a.zip",
    "https://github.com/user-attachments/assets/1/a.png",
    "https://github.com/owner/repo/releases/download/v1/a.zip",
    "https://github.com/owner/files/1/a.zip",
    "https://github.com/user-attachments/files/1/%E0%A4%A",
    "fixtures/identify-swd.tickwave",
    "",
  ]) {
    assert.equal(githubAttachment(src), null, src);
  }
});
