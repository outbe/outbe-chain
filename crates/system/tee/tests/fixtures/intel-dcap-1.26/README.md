# Intel DCAP 1.26 fixture provenance

These are immutable copies of the real SGX Processor-CA fixture distributed
with `dcap-qvl 0.5.2`:

- upstream source commit:
  `31a32a44de4cf68cb50c079e5bfd5348e4e6f4d5`;
- upstream repository:
  `https://github.com/Phala-Network/dcap-qvl`;
- upstream paths:
  `sample/sgx_quote` and `sample/sgx_quote_collateral.json`;
- upstream license: MIT;
- quote SHA-256:
  `f8b81014b6e443609746822194910f5dc1c92c322fa0584298d1e33e505ca3b5`;
- collateral-wrapper SHA-256:
  `bdd694bbe50f3a2a1cfe12f9e2bd83125921107a368edcf10780a5523b8501ce`.

The wrapper is test input only. Tests reconstruct the original Intel signed
TCB Info and QE Identity documents from its exact embedded strings before
calling the production raw-collateral interface. Production consensus evidence
does not use this wrapper format.
