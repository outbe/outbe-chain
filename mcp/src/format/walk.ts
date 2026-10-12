import type { AbiFunction, AbiParameter } from "viem";
import { type DecimalScale, type ReturnFormatContext, oraclePresentationScale } from "./oracle-scale.js";
import { formatScalar } from "./rules.js";

interface ParamFormatContext {
  contractName?: string;
  functionName?: string;
  enclosingTupleType?: string;
  marketDecimals?: DecimalScale;
}

/** A per-element scale applies to one array element; a single scale passes through. */
function elementScale(scale: DecimalScale | undefined, index: number): number | undefined {
  return Array.isArray(scale) ? scale[index] : scale;
}

/** A tuple or scalar inside a per-element table takes no scale of its own. */
function ownScale(scale: DecimalScale | undefined): number | undefined {
  return Array.isArray(scale) ? undefined : scale;
}

function formatArray(param: AbiParameter, value: unknown, context: ParamFormatContext): unknown {
  if (!Array.isArray(value)) return value;
  const element = { ...param, type: param.type.slice(0, -2) } as AbiParameter;
  return value.map((v, index) =>
    formatParam(element, v, { ...context, marketDecimals: elementScale(context.marketDecimals, index) }),
  );
}

function formatTuple(
  param: AbiParameter & { components: readonly AbiParameter[] },
  value: unknown,
  context: ParamFormatContext,
): Record<string, unknown> {
  const enclosingTupleType =
    "internalType" in param && typeof param.internalType === "string" ? param.internalType : context.enclosingTupleType;
  const out: Record<string, unknown> = {};
  param.components.forEach((component, index) => {
    const named = value && typeof value === "object" && component.name && component.name in (value as object);
    const sub = named ? (value as Record<string, unknown>)[component.name as string] : (value as unknown[])[index];
    out[component.name || String(index)] = formatParam(component, sub, {
      ...context,
      enclosingTupleType,
      marketDecimals: ownScale(context.marketDecimals),
    });
  });
  return out;
}

/** Recursively format a value against its ABI parameter metadata. */
export function formatParam(param: AbiParameter, value: unknown, context: ParamFormatContext = {}): unknown {
  if (param.type.endsWith("[]")) return formatArray(param, value, context);
  if (param.type === "tuple" && "components" in param && param.components) {
    return formatTuple(param as AbiParameter & { components: readonly AbiParameter[] }, value, context);
  }
  return formatScalar({
    name: param.name ?? "",
    type: param.type,
    value,
    context: {
      contractName: context.contractName,
      functionName: context.functionName,
      enclosingTupleType: context.enclosingTupleType,
      marketDecimals: ownScale(context.marketDecimals),
    },
  });
}

/** Humanize a decoded function result: a scalar for one output, an array for several. */
export function humanizeReturn(fn: AbiFunction, result: unknown, context?: ReturnFormatContext): unknown {
  const outputs = fn.outputs ?? [];
  const marketDecimals = oraclePresentationScale(fn, result, context);
  if (outputs.length === 0) return null;
  const paramContext = { contractName: context?.contractName, functionName: fn.name, marketDecimals };
  if (outputs.length === 1) {
    const formatted = formatParam(outputs[0], result, paramContext);
    return outputs[0].name ? { [outputs[0].name]: formatted } : formatted;
  }
  const out: Record<string, unknown> = {};
  outputs.forEach((p, i) => {
    out[p.name || String(i)] = formatParam(p, (result as unknown[])[i], paramContext);
  });
  return out;
}
