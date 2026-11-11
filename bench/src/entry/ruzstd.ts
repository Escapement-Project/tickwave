// escapement-tickwave-wasm (ruzstd), decoder-only build (--no-default-features).
import * as glue from "../../build/glue/ruzstd/escapement_tickwave_wasm.js";
import { wasmDecoder, type Glue } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(wasmDecoder(glue as unknown as Glue, new URL("./escapement_tickwave_wasm_bg.wasm", import.meta.url)));
