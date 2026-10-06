import type { Abi } from "viem";

/**
 * Generated precompile ABIs.
 *
 * The source of truth is `contracts/precompiles/src/I*.sol`. `mise run export-abi`
 * produces these JSON artifacts, and CI checks them for staleness.
 * Nothing here is hand-written. A signature can only change when the Solidity
 * changes. The registry uses each ABI whole and does not filter it.
 */
import IAgentReward from "../../contracts/precompiles/abi-export/IAgentReward.json";
import ICredis from "../../contracts/precompiles/abi-export/ICredis.json";
import IFidelity from "../../contracts/precompiles/abi-export/IFidelity.json";
import IGem from "../../contracts/precompiles/abi-export/IGem.json";
import IGemFactory from "../../contracts/precompiles/abi-export/IGemFactory.json";
import IGovernance from "../../contracts/precompiles/abi-export/IGovernance.json";
import IGratis from "../../contracts/precompiles/abi-export/IGratis.json";
import IMetadosis from "../../contracts/precompiles/abi-export/IMetadosis.json";
import INod from "../../contracts/precompiles/abi-export/INod.json";
import IOracle from "../../contracts/precompiles/abi-export/IOracle.json";
import IPayNote from "../../contracts/precompiles/abi-export/IPayNote.json";
import IPromis from "../../contracts/precompiles/abi-export/IPromis.json";
import IPromisLimit from "../../contracts/precompiles/abi-export/IPromisLimit.json";
import ISlashIndicator from "../../contracts/precompiles/abi-export/ISlashIndicator.json";
import IStaking from "../../contracts/precompiles/abi-export/IStaking.json";
import ITeeRegistryV1 from "../../contracts/precompiles/abi-export/ITeeRegistryV1.json";
import ITribute from "../../contracts/precompiles/abi-export/ITribute.json";
import ITributeFactory from "../../contracts/precompiles/abi-export/ITributeFactory.json";
import IValidatorSet from "../../contracts/precompiles/abi-export/IValidatorSet.json";
import IZeroFee from "../../contracts/precompiles/abi-export/IZeroFee.json";

const asAbi = (json: unknown): Abi => json as Abi;

export const PRECOMPILE_ABI = {
  IAgentReward: asAbi(IAgentReward),
  ICredis: asAbi(ICredis),
  IFidelity: asAbi(IFidelity),
  IGem: asAbi(IGem),
  IGemFactory: asAbi(IGemFactory),
  IGovernance: asAbi(IGovernance),
  IGratis: asAbi(IGratis),
  IMetadosis: asAbi(IMetadosis),
  INod: asAbi(INod),
  IOracle: asAbi(IOracle),
  IPayNote: asAbi(IPayNote),
  IPromis: asAbi(IPromis),
  IPromisLimit: asAbi(IPromisLimit),
  ISlashIndicator: asAbi(ISlashIndicator),
  IStaking: asAbi(IStaking),
  ITeeRegistryV1: asAbi(ITeeRegistryV1),
  ITribute: asAbi(ITribute),
  ITributeFactory: asAbi(ITributeFactory),
  IValidatorSet: asAbi(IValidatorSet),
  IZeroFee: asAbi(IZeroFee),
} as const;
