#!/usr/bin/env bb
;; Loads the transit example pod and checks that Clojure values survive the
;; round trip with their types.
;;
;;   cargo build --example transit_pod
;;   bb examples/transit.bb           # stdin/stdout transport
;;   bb examples/transit.bb socket    # localhost socket transport

(require '[babashka.pods :as pods])

(def transport (if (= "socket" (first *command-line-args*)) :socket :stdio))
(def pod-path (or (System/getenv "TRANSIT_POD") "target/debug/examples/transit_pod"))

(pods/load-pod [pod-path] (cond-> {} (= :socket transport) (assoc :transport :socket)))

(require '[pod.joshrotenberg.transit :as tr])

(defn show [label v]
  (println (str label ":") (pr-str v) (pr-str (type v))))

(println "transport:" transport)
(show "tag-set" (tr/tag-set [:b :a :b]))
(println "tag-set is a set of keywords:" (let [s (tr/tag-set [:x :y])] (and (set? s) (every? keyword? s))))
(show "kw-info" (tr/kw-info :my.ns/thing))
(show "plus-days" (tr/plus-days #inst "2026-10-14T00:00:00.000-00:00" 1))
(show "point" (tr/point 3 4))
(show "ping" (tr/ping))
(let [xs [:a :ns/b 'sym \x 1/3 1.5M 12345678901234567890N 1N '(1 2) [1 2] #{:s} {:a 1 "a" 2} {1 :one}
          #uuid "550e8400-e29b-41d4-a716-446655440000" #inst "2026-01-01T00:00:00.000-00:00" nil]
      back (mapv tr/echo xs)]
  (println "echo:" (pr-str back))
  ;; babashka's transit reader reads a list as a seq (LazySeq), so a list is
  ;; checked with seq? and not by its class.
  (println "echo keeps types:"
           (every? true? (map #(and (= %1 %2) (if (seq? %1) (seq? %2) (= (type %1) (type %2)))) xs back))))
(show "meta-of" (tr/meta-of ^{:source :repl} {:a 1}))
(show "meta-of plain" (tr/meta-of {:a 1}))
(try
  (tr/fail :boom)
  (catch clojure.lang.ExceptionInfo e
    (println "fail:" (ex-message e) (pr-str (ex-data e)))))
(let [seen (atom [])]
  (tr/ticks 3 #(swap! seen conj %))
  (println "ticks:" (pr-str @seen)))
