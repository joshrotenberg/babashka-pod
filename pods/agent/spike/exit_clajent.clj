(require '[clajent.core :as agent])
(def mode (first *command-line-args*))
(def req {:provider (keyword (second *command-line-args*)) :cwd (System/getProperty "user.dir")
          :prompt "Write a 3000 word essay about glaciers, marker ZQXJ"})
(def req (cond-> req (nth *command-line-args* 2 nil) (assoc :model (nth *command-line-args* 2))))
(case mode
  "start-then-exit" (do (agent/start! req) (Thread/sleep 2500) (println "exiting with turn running"))
  "run-block" (do (println "running") (flush) (agent/run! req)))
