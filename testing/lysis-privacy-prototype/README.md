# Исследование приватного Tribute → Lysis → Nod → Gratis

Актуальное состояние на 2026-09-11: исследование и замечания трёх проверок дополнены [исполняемым no-TEE PoC](poc/README.md). Завершены локальные сквозные сценарии на 4 и 256 разных Tribute, реальные proofs, две ротации и private MPC. **P_link для 32 SU проходит 512 МБ на host; 64 SU превышает лимит.** [Замеры и ограничения масштаба](poc/RESULTS.md), [кто считает/хранит и покрытие R00–R18](poc/COVERAGE_AND_STORAGE.md). Production security, полный набор adapters и TPS сети не подтверждены.

Порядок чтения:

1. [Замечания, выводы и внесённые изменения](REVIEW_REMEDIATION.md): каждый IR-01–IR-06, проверка основания, исправление и остаточный статус.
2. [Требования и трассировка R00–R18](PROTOCOL_TRACE_AND_REQUIREMENTS.md): принятые условия, текущий код, входы/выходы, роли и хранение.
3. [Подробный downstream trace](DOWNSTREAM_PRIVATE_STATE_TRACE.md): Intex payouts, collateral writers, Promis→Gratis, supply и Fidelity query.
4. [Исследование методов и библиотек](DEEP_RESEARCH_IMPLEMENTATION.md): P_link/prover adapters, агрегаты, exact-time Fidelity, Nod/Gratis, размеры и план измерений.
5. [Границы готовности](RESEARCH_COMPLETION_REVIEW.md) и [подробный протокол агрегатов](DEEP_RESEARCH_AGGREGATION.md).

[Исходные три независимых отчёта](independent-review-2026-09-11/README.md) сохранены с их первоначальным snapshot; они не переписаны после исправлений.

[Версии библиотек](DEEP_RESEARCH_LIBRARY_PINS.json), [арифметическая проверка](DEEP_RESEARCH_ARITHMETIC.json), [реестр проверенных артефактов](DEEP_RESEARCH_EVIDENCE.json).

Другие Markdown-файлы и каталог [measurements](measurements/README.md) содержат предшествующие исследования и эксперименты. Их прежние параметры, timings и схемы не являются принятым текущим протоколом. SEAL исключён из shortlist; старые результаты сохранены как история.
