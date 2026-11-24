// Small hierarchical test netlist, written by hand with ASAP7 cells: a registered 2-bit adder and a parity unit.
// hier_top -> u_add (add2) -> fa0, fa1 (fa) -> h0, h1 (ha), and hier_top -> u_par (par)
// plus a 4-bit bus between two boxes: din -> u_buf (buf4) -> mid[3:0] -> u_neg (inv4) -> dout
module ha (a, b, s, c);
  input a, b;
  output s, c;
  XOR2xp5_ASAP7_75t_R x (.A(a), .B(b), .Y(s));
  AND2x2_ASAP7_75t_R n (.A(a), .B(b), .Y(c));
endmodule

module fa (a, b, ci, s, co);
  input a, b, ci;
  output s, co;
  wire s1, c1, c2;
  ha h0 (.a(a), .b(b), .s(s1), .c(c1));
  ha h1 (.a(s1), .b(ci), .s(s), .c(c2));
  OR2x2_ASAP7_75t_R o (.A(c1), .B(c2), .Y(co));
endmodule

module add2 (a, b, ci, s, co);
  input [1:0] a, b;
  input ci;
  output [1:0] s;
  output co;
  wire c0;
  fa fa0 (.a(a[0]), .b(b[0]), .ci(ci), .s(s[0]), .co(c0));
  fa fa1 (.a(a[1]), .b(b[1]), .ci(c0), .s(s[1]), .co(co));
endmodule

module par (d, p);
  input [2:0] d;
  output p;
  wire t;
  XOR2xp5_ASAP7_75t_R x0 (.A(d[0]), .B(d[1]), .Y(t));
  XOR2xp5_ASAP7_75t_R x1 (.A(t), .B(d[2]), .Y(p));
endmodule

module buf4 (i, o);
  input [3:0] i;
  output [3:0] o;
  BUFx2_ASAP7_75t_R b0 (.A(i[0]), .Y(o[0]));
  BUFx2_ASAP7_75t_R b1 (.A(i[1]), .Y(o[1]));
  BUFx2_ASAP7_75t_R b2 (.A(i[2]), .Y(o[2]));
  BUFx2_ASAP7_75t_R b3 (.A(i[3]), .Y(o[3]));
endmodule

module inv4 (i, o);
  input [3:0] i;
  output [3:0] o;
  INVx1_ASAP7_75t_R n0 (.A(i[0]), .Y(o[0]));
  INVx1_ASAP7_75t_R n1 (.A(i[1]), .Y(o[1]));
  INVx1_ASAP7_75t_R n2 (.A(i[2]), .Y(o[2]));
  INVx1_ASAP7_75t_R n3 (.A(i[3]), .Y(o[3]));
endmodule

module hier_top (clk, a, b, ci, q, qp, din, dout);
  input clk;
  input [1:0] a, b;
  input ci;
  output [2:0] q;
  output qp;
  input [3:0] din;
  output [3:0] dout;
  wire [3:0] mid;
  wire [1:0] s;
  wire co, p, pn;
  wire [2:0] qn;
  add2 u_add (.a(a), .b(b), .ci(ci), .s(s), .co(co));
  par u_par (.d({co, s}), .p(p));
  buf4 u_buf (.i(din), .o(mid));
  inv4 u_neg (.i(mid), .o(dout));
  DFFHQNx1_ASAP7_75t_R r0 (.CLK(clk), .D(s[0]), .QN(qn[0]));
  DFFHQNx1_ASAP7_75t_R r1 (.CLK(clk), .D(s[1]), .QN(qn[1]));
  DFFHQNx1_ASAP7_75t_R r2 (.CLK(clk), .D(co), .QN(qn[2]));
  DFFHQNx1_ASAP7_75t_R rp (.CLK(clk), .D(p), .QN(pn));
  INVx1_ASAP7_75t_R i0 (.A(qn[0]), .Y(q[0]));
  INVx1_ASAP7_75t_R i1 (.A(qn[1]), .Y(q[1]));
  INVx1_ASAP7_75t_R i2 (.A(qn[2]), .Y(q[2]));
  INVx1_ASAP7_75t_R ip (.A(pn), .Y(qp));
endmodule
