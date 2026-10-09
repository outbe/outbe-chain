
/**
 * Generated precompile ABIs.
 *
 * The source of truth is `contracts/precompiles/src/I*.sol`. `mise run export-abi`
 * produces these JSON artifacts, and CI checks them for staleness.
 * Nothing here is hand-written. A signature can only change when the Solidity
 * changes. The registry uses each ABI whole and does not filter it.
 */
import IAgentReward from "./abi/generated/precompiles/IAgentReward.js";
import ICredis from "./abi/generated/precompiles/ICredis.js";
import ICredisFactory from "./abi/generated/precompiles/ICredisFactory.js";
import IFidelity from "./abi/generated/precompiles/IFidelity.js";
import IGem from "./abi/generated/precompiles/IGem.js";
import IGemFactory from "./abi/generated/precompiles/IGemFactory.js";
import IGovernance from "./abi/generated/precompiles/IGovernance.js";
import IGratis from "./abi/generated/precompiles/IGratis.js";
import IGratisFactory from "./abi/generated/precompiles/IGratisFactory.js";
import IMetadosis from "./abi/generated/precompiles/IMetadosis.js";
import INod from "./abi/generated/precompiles/INod.js";
import IOracle from "./abi/generated/precompiles/IOracle.js";
import IPromis from "./abi/generated/precompiles/IPromis.js";
import IPromisLimit from "./abi/generated/precompiles/IPromisLimit.js";
import ISlashIndicator from "./abi/generated/precompiles/ISlashIndicator.js";
import IStaking from "./abi/generated/precompiles/IStaking.js";
import ITeeRegistryV1 from "./abi/generated/precompiles/ITeeRegistryV1.js";
import ITribute from "./abi/generated/precompiles/ITribute.js";
import ITributeFactory from "./abi/generated/precompiles/ITributeFactory.js";
import IValidatorSet from "./abi/generated/precompiles/IValidatorSet.js";
import IVaultRouter from "./abi/generated/precompiles/IVaultRouter.js";
import IZeroFee from "./abi/generated/precompiles/IZeroFee.js";

export const PRECOMPILE_ABI = {
  IAgentReward,
  ICredis,
  ICredisFactory,
  IFidelity,
  IGem,
  IGemFactory,
  IGovernance,
  IGratis,
  IGratisFactory,
  IMetadosis,
  INod,
  IOracle,
  IPromis,
  IPromisLimit,
  ISlashIndicator,
  IStaking,
  ITeeRegistryV1,
  ITribute,
  ITributeFactory,
  IValidatorSet,
  IVaultRouter,
  IZeroFee,
} as const;
