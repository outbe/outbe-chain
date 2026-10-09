import type { Abi } from "viem";
import RouterJson from "../../../contracts/intent/abi-export/Router.json";

/**
 * ABI + constants for the intent (ERC-7683 Router) tools.
 *
 * ABIs are generated, not hand-written - see `src/abi.ts`. Source of truth:
 *  - contracts/intent/src/router/... via contracts/intent/abi-export/Router.json
 *  - contracts/tokens/src/interfaces/IERC20.sol
 */

export const DEFAULT_ROUTER = "0xC846a86D4FE91a43E900a7a3bd5BE23ED2C30492";
export const DEFAULT_FILL_DEADLINE_SECONDS = 120;

export const ROUTER_ABI: Abi = RouterJson as Abi;

export { ERC20_ABI } from "../net/erc20.js";
