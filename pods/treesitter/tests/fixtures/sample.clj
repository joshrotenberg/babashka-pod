(ns example.core
  "A fixture for the outline tests."
  (:require [clojure.string :as str]))

(def limit 10)

(defonce state (atom {}))

(defn add
  "Adds two numbers."
  [a b]
  (+ a b))

(defn- ^:private shout [s]
  (str/upper-case s))

(defn area
  ([w] (area w w))
  ([w h] (* w h)))

(defmacro unless [test & body]
  `(when-not ~test ~@body))

(defmulti describe :kind)

(defmethod describe :circle [shape]
  (str "circle " (:r shape)))

(defprotocol Shape
  (perimeter [this]))

(defrecord Square [side]
  Shape
  (perimeter [_] (* 4 side)))

(comment
  (def scratch 1))
