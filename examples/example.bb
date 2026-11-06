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
(println "add no args:" (ex/add))
(println "ping:" (ex/ping))
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

;; A pod call inside the callback. The callback runs on the calling thread,
;; not babashka's pod reader thread, so this does not deadlock.
(let [seen (atom [])
      done (future (ex/count-up 2 (fn [i] (swap! seen conj [i (ex/ping)]))))]
  (println "count-up nested:" (deref done 5000 :timeout) @seen))

(try
  (ex/count-up 5 (fn [i] (when (= i 1) (throw (ex-info "callback failed" {:at i})))))
  (catch clojure.lang.ExceptionInfo e
    (println "callback error:" (ex-message e) (ex-data e))))
(println "after callback error:" (ex/ping))

(println "stream doc:" (:doc (meta #'ex/count-up)))
(try
  (ex/count-up "x" identity)
  (catch clojure.lang.ExceptionInfo e
    (println "stream error:" (ex-message e))))
(try
  (ex/count-up 3)
  (catch clojure.lang.ExceptionInfo e
    (println "no callback:" (ex-message e))))

(let [c (ex/counter 10)]
  (println "counter:" (pr-str c))
  (ex/inc! c)
  (println "inc!:" (ex/inc! c))
  (ex/close! c)
  (try
    (ex/inc! c)
    (catch clojure.lang.ExceptionInfo e
      (println "closed:" (ex-message e) (pr-str (ex-data e))))))

(try
  (ex/add "x")
  (catch clojure.lang.ExceptionInfo e
    (println "bad args:" (ex-message e))))
