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
(show "meta-of" (tr/meta-of ^{:source :repl} {:a 1}))
(show "meta-of plain" (tr/meta-of {:a 1}))
(try
  (tr/fail :boom)
  (catch clojure.lang.ExceptionInfo e
    (println "fail:" (ex-message e) (pr-str (ex-data e)))))
(let [seen (atom [])]
  (tr/ticks 3 #(swap! seen conj %))
  (println "ticks:" (pr-str @seen)))
