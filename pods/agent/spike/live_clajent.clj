(require '[clajent.core :as agent])

(def provider (keyword (or (first *command-line-args*) "claude")))
(def model (second *command-line-args*))
(def cwd (System/getProperty "user.dir"))
(defn t [] (System/currentTimeMillis))
(defn brief [r] (select-keys r [:status :text :data :session :cost-usd :duration-ms :error]))
(defn req [prompt & {:as more}] (merge (cond-> {:provider provider :cwd cwd :prompt prompt} model (assoc :model model)) more))

(let [t0 (t) r (agent/run! (req "Reply with exactly the word: apple"))
      session (:session r)]
  (println "run 1:" (pr-str (brief r)) "wall-ms" (- (t) t0))
  (println "run 2 (resumed):" (pr-str (brief (agent/run! (req "What word did you reply with last time? Reply with just that word." :session session)))))

  (let [seen (atom [])
        turn (agent/start! (req "Count from 1 to 5, one number per line." :session session)
                           {:on-event #(swap! seen conj (or (get % "type") (:type %) (some-> % :native (get "type"))))})
        polls (loop [n 0] (if (realized? turn) n (do (Thread/sleep 100) (recur (inc n)))))]
    (println "start!: polled" polls "times; snapshot" (pr-str (select-keys (agent/snapshot turn) [:phase :status])))
    (println "start! result:" (pr-str (brief @turn)))
    (Thread/sleep 200)
    (println "events seen:" (count @seen) (pr-str (frequencies @seen))))

  (let [r (agent/run! (req "Give the capital of France."
                           :session session
                           :output-schema {"type" "object" "properties" {"city" {"type" "string"}}
                                           "required" ["city"] "additionalProperties" false}))]
    (println "schema:" (pr-str (select-keys r [:status :data :error]))))

  (let [turn (agent/start! (req "Write a 2000 word essay about rivers." :session session))]
    (Thread/sleep 3000)
    (agent/cancel! turn)
    (let [t0 (t) r (deref turn 20000 :timeout)]
      (println "cancel:" (pr-str (if (map? r) (select-keys r [:status :error]) r)) "settled-ms" (- (t) t0))))

  (let [r (agent/run! (req "Write a 2000 word essay about mountains." :session session :timeout-ms 3000))]
    (println "timeout:" (pr-str (select-keys r [:status :error])))))
(println "done")
