// DecompressionStream("zstd") and the TypeScript reader.
import { streamDecoder } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(streamDecoder());
