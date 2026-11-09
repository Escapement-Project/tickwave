// fzstd and the TypeScript reader with a pure-JavaScript XXH64 (@intrnl/xxhash64, BigInt).
import { fzstdDecoder, pureJsXxh } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(fzstdDecoder(async () => pureJsXxh));
