#!/usr/bin/env python3
"""Render measured public evidence; no estimates substituted for execution."""
import argparse
import json
from pathlib import Path

def read(p): return json.loads(p.read_text())
def num(x, digits=2): return f"{x:.{digits}f}".replace(".", ",")
def main():
    p=argparse.ArgumentParser();p.add_argument("--evidence",type=Path,required=True)
    a=p.parse_args();e=a.evidence.resolve();root=e.parents[1]
    result=read(e/"result.json");comparison=read(e/"paired-baseline/comparison.json")
    storage=read(e/"storage.json");parameters=read(e/"parameters.json")
    timings=read(e/"timings.json");worker=read(e/"worker.json")
    rows={r["operation"]:r for r in comparison["rows"]}
    sizes={r["operation"]:r for r in storage["operations"]}
    def metric(stage):
        paths=list((e/"metrics").glob(f"*-{stage}.json"))
        if len(paths)!=1:raise ValueError((stage,len(paths)))
        return read(paths[0])
    cold=comparison["source"]["ristretto_cold"];oldsource=comparison["source"]["baseline_cold"]
    batch=read(e/"link-generate.json");work=sum(x["work_ms"] for x in batch["parts"])/1000
    mbatch=metric("warm-distinct-link");node=read(e/"link-verify.json")
    labels={"claim":"Claim Nod", "payment":"Private payment/split", "receive":"Receive",
            "withdraw":"Gratis → COEN", "promis":"PROMIS → Gratis", "pledge-1":"Pledge",
            "release-1":"Release", "cross-send":"Cross-owner sender", "cross-receive":"Cross-owner recipient",
            "intex-cashout":"Intex cashout"}
    lines=["# Полный Ristretto: результаты исправленного варианта 2", "",
      f"2026-09-12. Исполнено **{result['distinct_tributes']} разных TributeOffer × {result['su_per_offer']} SU**, настоящие P_L2/P_link, две ротации VSS, S/S_l, Lysis/Nod, приватный Gratis, два владельца, COEN и закрытая история. `experimental_lifecycle_executed=true`; production protocol pass — false.", "",
      "**Baby-Jubjub и cross-group bridges удалены из нового приватного backend.** Commitments nominal/Nod/Gratis и VSS используют Ristretto255; денежный слой использует Bulletproofs и Σ protocols. Groth16/BN254 остаётся в P_link для canonical source и доказательства Ristretto opening. Это исполненная полная замена группы приватного состояния, а не прежний гибрид.", "",
      "[Baseline](../../RESULTS.md), [сохранённый гибрид](../RESULTS.md), [схема и роли](README.md), [парные исходные измерения](results/host-2026-09-12/paired-baseline/comparison.json). Все KB/MB десятичные. Host/native; телефон и WASM не измерялись. Один sample на операцию, без статистического утверждения и sustained network TPS.", "",
      "## Кошелёк и нода: парные числовые входы", "",
      "Baseline заново доказывает те же числовые witnesses и коэффициенты. Blinders/commitments заново создаются для соответствующей группы. Cold включает PK/lookup/generator initialization, proving, self-verify и serialization; compile/setup исключены. Проверка нодой — подготовленный VK у baseline и подготовленные generators у Ristretto. Cold node cost приведён отдельно; чтение registry/bundle и consensus не включены.", "",
      "| Операция | Baseline cold, с | Ristretto cold, с | Baseline RSS, MB | Ristretto RSS, MB | Baseline node, мс | Ristretto node warm, мс | Ristretto node cold, мс |",
      "|---|---:|---:|---:|---:|---:|---:|---:|"]
    for tag,label in labels.items():
        r=rows[tag]
        vs=[r['baseline_cold_seconds'],r['ristretto_cold_seconds'],r['baseline_peak_rss_bytes']/1e6,r['ristretto_peak_rss_bytes']/1e6,r['baseline_node_prepared_verify_ms'],r['ristretto_node_prepared_verify_ms'],r['ristretto_node_cold_verify_ms']]
        lines.append(f"| {label} | "+" | ".join(num(x,3 if i<2 else 2) for i,x in enumerate(vs))+" |")
    lines += ["", "Cross-owner строки измеряют два локальных денежных statements, а не полную latency перевода. Dual handles, recovery, signatures, два history gates и commit идут дополнительно в timings. Baseline не выдаётся за ранее реализованный cross-owner protocol.", "",
      "## Размер proof и полного пакета", "",
      "Baseline Groth16 proof — **128 B**. Новый bundle содержит public statement, ciphertexts и составное доказательство. Эти размеры различаются по смыслу; ниже они разделены. Proof/auxiliary column включает framing, дополнительные commitments и ciphertext положительности, используемые внутри доказательства.", "",
      "| Операция | Baseline эквивалент statement + proof, B | Ristretto statement, B | Основные ciphertexts, B | Proof/auxiliary, B | Полный bundle, B |",
      "|---|---:|---:|---:|---:|---:|"]
    for tag,label in labels.items():
        s=sizes[tag]
        lines.append(f"| {label} | {s['baseline_equivalent_statement_plus_proof_bytes']} | {s['statement_wire_bytes']} | {s['primary_ciphertexts_wire_bytes']} | {s['proofs_and_auxiliary_objects_wire_bytes']} | {s['bundle_bytes']} |")
    lines += ["", "Baseline tuple — сопоставимый bincode codec, не deployed wire format. Новые размеры сверены с фактически записанным префиксом каждого `twisted.bin`. Подписи, VSS packets, receipts и источник идут дополнительно. У первой Claim доказываются и первоначальные funding bindings.", "",
      "Один uint256 ciphertext: **1 024 B raw / 1 072 B с key и lengths**. Четыре Ristretto note commitments: **128 B**. Они представляют ту же сумму в одной группе: 64-bit limbs нужны текущему VSS/history, 16-bit chunks — key-only recovery. Их связывают четыре небольших Σ proofs; прежнего 87 336-B Baby/Ristretto bridge нет. Объединение этих двух limb layouts в один здесь не измерялось.", "",
      "## Цена полного переноса P_link", "",
      "| Метрика | Baseline | Полный Ristretto |", "|---|---:|---:|",
      f"| Cold P_link32 на одинаковом source | {num(oldsource['wall_seconds'],3)} с | {num(cold['wall_seconds'],3)} с |",
      f"| Cold prover peak RSS | {num(oldsource['peak_rss_bytes']/1e6)} MB | {num(cold['peak_rss_bytes']/1e6)} MB |",
      "| P_link proof bytes | 128 | 1 664 = 13 × 128 |",
      f"| Новый профиль256, сумма proof work | — | {num(work)} с; {num(256/work,3)} complete P_link/с |",
      f"| Новый профиль256, включая13 PK loads/processes | — | {num(mbatch['wall_seconds'])} с; {num(mbatch['peak_rss_bytes']/1e6)} MB peak |",
      f"| Проверка всех256 полных P_link | — | {num(node['batch_ms'])} мс |",
      f"| Общие source PK на диске | — | {parameters['pk_disk_bytes']:,} B |",
      f"| Общие source VK на диске | — | {parameters['vk_disk_bytes']:,} B |", "",
      "Cold Ristretto — сумма13 последовательных свежих native processes; RSS — максимум их high-water marks. Оркестрация считается отдельно; это не измерение resident memory всей мобильной UI. Каждый part прошёл512 000 000 B. Такой host способ освобождает allocator arenas между PK; перенос механизма на мобильный runtime ещё предстоит.", "",
      "P_link стал дороже из-за foreign-field арифметики Ristretto внутри Groth16. Конструкция использует одну source proof и12 скрытых шагов opening, salted state digests и обязательную проверку полного набора. Ещё416B raw занимают13 digests,32B — opening binding; фактический JSON больше. Монолитный probe был остановлен по RAM; он не объявляется невозможным для всех других реализаций.", "",
      "## Lysis, агрегаты и закрытая история", "",
      f"Lysis kernel + descriptor worker после готовности агрегатов/Fidelity: **{num(worker['elapsed_with_coefficient_kernel_ms'])} мс**. Nod descriptor —278B; {result['distinct_tributes']} records записано в {sum(s['bytes'] for s in worker['shards'])}B. Это локальная обработка с fsync/SQLite, без consensus/OCOMP.", "",
      "VSS сохранился, но группа и поле долей перенесены на Ristretto. После close открываются S/S_l. Каждый источник ограничен текущим104-bit codec; миллиард таких источников даёт сумму<2^134<q_R. uint256 balances/operations доказываются по limbs; произвольный внешний uint256 source вне этого codec потребует отдельного profile.", "",
      "| MPC consumer | Wall, с | Framed traffic, MB | Max actor RSS, MB |", "|---|---:|---:|---:|"]
    for t in timings:
        if 'framed_sent_bytes' in t:
            lines.append(f"| {t['stage']} | {num(t['seconds'])} | {num(t['framed_sent_bytes']/1e6)} | {num(t['max_party_rss']/1e6)} |")
    lines += ["", "Точные Fidelity/LIFO/Intex/expiry формулы сохранены. Их стоимость не исчезла после смены денежного prover. Для ориентира сохранённый baseline: Intex2 —119,96с/153,72MB, expiry255 —46,76с/94,61MB. Разница единичных host samples сама по себе не доказывает ускорение MPC.", "",
      "## Кто хранит данные", "",
      f"Tribute artifacts в полном запуске: среднее **{num(result['tribute_artifact_bytes']['mean'])}B**, минимум{result['tribute_artifact_bytes']['min']}B. Первый Tribute содержит дополнительные genesis cohorts. Это формат PoC с SU/metadata/VSS/receipts, а не минимальный production codec.", "",
      f"Реестр содержит {storage['cipher_registry_entries']} note/cipher bindings и занимает{storage['cipher_registry_json_bytes']}B JSON. Это retained history конкретного harness; raw ciphertext size нельзя подменять этим JSON объёмом.", "",
      "Кошелёк хранит encryption key, source witnesses/blinders/соль и необходимые history recovery данные; сумма ciphertext восстанавливается только по key и публичным chunks. Node хранит commitments, ciphertexts, proofs и версии/права. VSS holders хранят свои доли и handoff state. Приватные границы ролей, дополнительные recovery witnesses и remaining production adapters описаны в README. Не все private witnesses заменяются одним DK.", "",
      "## Проверки и вывод", "",
      "Пройден полный256 lifecycle и отдельный private correctness oracle. Пять Rust protocol tests проверяют Ristretto VSS/note links, integer/scalar alias, full-width conservation/multiplication/overflow, source104 и связывание source/final opening. Семь public source packet cases включают корректный packet и отклонение пропущенной/переставленной части, лишнего байта, изменённых digest/binding/owner. Два публичных samples повторно проверяются без private witnesses; verdict и source/parameter hashes находятся в evidence.", "",
      "Полный Ristretto устраняет дорогостоящие cross-group bridges и снижает расходы денежного кошелька относительно baseline. Составные Bulletproofs/Σ остаются крупнее и медленнее для verifier, чем128-B Groth16; перенос исходного P_link добавляет существенную стоимость. Поэтому измерения не подтверждают универсального выигрыша полного backend или готовности к миллиарду Tribute.", "",
      "Ограничения: single-party test setup, собственный неаудированный foreign-field gadget, hash-hiding salted composition, passive honest-majority MPC, локальный транспорт и trusted controller/VK paths. Production authority/funding, malicious/adaptive MPC, DKG/VSS transport, DA/consensus, безопасное стирание, key rotation, произвольный SU и offline immediate receiver credit не подтверждены. Creation разных Tribute может выполняться независимыми кошельками/процессами; parallel throughput нового prover этим запуском не измерялся.", "",
      "[Public evidence manifest](results/host-2026-09-12/MANIFEST.json), [result](results/host-2026-09-12/result.json), [storage](results/host-2026-09-12/storage.json), [timings](results/host-2026-09-12/timings.json), [source review](../RISTRETTO_SOURCE_RESEARCH.md). Baseline и код гибрида сохранены по hashes.", ""]
    (root/"RESULTS.md").write_text("\n".join(lines))
    print(root/"RESULTS.md")

if __name__=="__main__":main()
