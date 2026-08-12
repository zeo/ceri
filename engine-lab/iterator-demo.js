const values = ["custom", "iterator"];
let index = 0;
const iterable = {
  [Symbol.iterator]() {
    return {
      next() {
        if (index < values.length) {
          const value = values[index];
          index = index + 1;
          return { value: value, done: false };
        }
        return { done: true };
      },
    };
  },
};

let output = "";
for (const value of iterable) {
  output = output + value + " ";
}
document.getElementById("status").textContent = output;
