// escapement-tickwave-wasm (ruzstd), default build, with the sidecar parser.
import * as glue from "../../build/glue/ruzstd-full/escapement_tickwave_wasm.js";
import { wasmDecoder, type Glue } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(wasmDecoder(glue as unknown as Glue, new URL("./escapement_tickwave_wasm_bg.wasm", import.meta.url)));
