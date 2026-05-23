module Greetable
  def greet
    "Hello"
  end
end

class Person
  include Greetable
  attr_accessor :name

  def initialize(name)
    @name = name
  end
end

p = Person.new("World")
puts p.greet
