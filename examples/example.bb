#!/usr/bin/env bb
;; Loads the example pod and calls each of its functions.
;;
;;   cargo build --example example_pod
;;   bb examples/example.bb           # stdin/stdout transport
;;   bb examples/example.bb socket    # localhost socket transport

(require '[babashka.pods :as pods])

(def transport (if (= "socket" (first *command-line-args*)) :socket :stdio))
(def pod-path (or (System/getenv "EXAMPLE_POD") "target/debug/examples/example_pod"))

(pods/load-pod [pod-path] (cond-> {} (= :socket transport) (assoc :transport :socket)))

(require '[pod.joshrotenberg.example :as ex])

(println "transport:" transport)
(println "add:" (ex/add 1 2 3.5))
(println "greet:" (ex/greet "babashka"))
(println "doc:" (:doc (meta #'ex/add)))

(println "divide:" (ex/divide 10 3))
(try
  (ex/divide 1 0)
  (catch clojure.lang.ExceptionInfo e
    (println "divide error:" (ex-message e) (ex-data e))))

(let [seen (atom [])]
  (ex/count-up 5 #(swap! seen conj %))
  (println "count-up:" @seen))

(try
  (ex/add "x")
  (catch clojure.lang.ExceptionInfo e
    (println "bad args:" (ex-message e))))
