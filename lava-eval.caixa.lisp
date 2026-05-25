(defcaixa
  :name
  "lava-eval"
  :kind
  :Biblioteca
  :ecosystem
  :rust-single-crate
  :package
  {:name "lava-eval"
   :version "0.1.0"
   :description "In-memory tatara-lisp evaluator for lava architectures. Parses .tlisp source, evaluates (deflava-architecture ...) forms, produces typed lava_core::Architecture. Magma consumes this directly to do plan/apply in-memory — no on-disk JSON between authoring and apply. Extracted from lava-architectures for reuse by magma-lava + future consumers."
   :license "MIT"
   :repository "https://github.com/pleme-io/lava-eval"}
  :ci-config
  {:bump {:default-type "patch"}
   :publish {:no-verify true}}
  :workflows
  [:auto-release :pre-merge-gate :security-gate])
