// Package sample is a fixture for the outline tests.
package sample

import "math"

const Limit = 10

var greeting = "hi"

// Point is a point.
type Point struct {
	X, Y int
}

type Shape interface {
	Area() float64
}

type Celsius float64

type Circle struct {
	R float64
}

func (c *Circle) Area() float64 {
	return math.Pi * c.R * c.R
}

func Add(a, b int) int {
	return a + b
}
