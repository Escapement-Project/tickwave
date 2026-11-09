// fzstd, the TypeScript reader, and hash-wasm's XXH64 for the checksum fzstd skips.
import { fzstdDecoder, hashWasmXxh } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(fzstdDecoder(hashWasmXxh));
